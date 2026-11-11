<script lang="ts">
  import { onMount, tick } from "svelte";
  import { popover, restoreFocusTo, popoverExit } from "../ui/popover";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { repoStore } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
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
  import { ShieldCheck, ShieldAlert, ShieldQuestion, Sparkles, RefreshCw, CircleAlert } from "@lucide/svelte";

  /** Whether the Manvi menu is open. Bindable so a host (or a test) can open it. */
  let { open = $bindable(false) }: { open?: boolean } = $props();

  const instanceId = $props.id();
  let chipEl = $state<HTMLButtonElement | null>(null);
  let menuEl = $state<HTMLElement | null>(null);

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
    open = false;
    requestManviFocus(target);
    repoStore.setActiveTab("work", "policy");
  }

  /** Focus moves into the menu on open so the keyboard lands on the model list. */
  function toggleMenu() {
    open = !open;
    if (open) void tick().then(() => menuEl?.querySelector<HTMLElement>("select")?.focus());
  }

  /**
   * Anchored under the chip and right-aligned to it, since the cluster sits at
   * the window's right. Escape returns focus to the chip; the chip counts as
   * inside, or its own pointerdown would close the menu before its click.
   */
  const menuPopover = $derived({
    anchor: { kind: "element" as const, element: chipEl, gap: 6, align: "end" as const },
    dismiss: {
      inside: "[data-manvi-menu], [data-manvi-chip]",
      escape: "capture" as const,
      resize: true,
    },
    onDismiss: (reason: string) => {
      open = false;
      if (reason === "escape") restoreFocusTo(chipEl);
    },
  });

  // Manvi is a repository view: with no repository open there is no session to
  // switch tabs on, so navigation chips remain disabled until a repo is open.
  let reachable = $derived(Boolean($repoStore.currentPath));
  let unreachableHint = "Open a repository to reach the Manvi view.";

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
  /**
   * Closed title-bar label. A failed model check reads "Failed"; the icon
   * button beside it is the retry. The failure sentence stays on the tooltip.
   */
  let closedFace = $derived(
    $harnessStore.isProbing
      ? "Checking…"
      : $harnessStore.error
        ? "Failed"
        : preferred
          ? preferred.model
          : modelReady && ai?.selected
            ? `Automatic · ${ai.selected.model}`
            : ai
              ? "No models"
              : "Not checked",
  );
  /** The Automatic choice. When it is selected, its label is the closed face. */
  let automaticOption = $derived(preferred ? "Automatic" : closedFace);
  let discoveryNote = $derived(
    endpoints.some((endpoint) => endpoint.models.length > 0)
      ? ""
      : $harnessStore.isProbing
        ? "Looking for local models…"
        : "No local models available",
  );
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

  /** The chip's accessible name keeps "Manvi" and the state when only the shield shows. */
  let harnessStateWord = $derived(
    permissionMode === "connected" ? "policy checks connected"
      : permissionMode === "blocked" ? "policy checks failing"
        : permissionMode === "unguarded" ? "not installed"
          : "not checked",
  );
  let chipLabel = $derived(`Manvi, ${harnessStateWord}`);
  /** A fired rule or an unchecked action never hides behind the menu. */
  let verdictInline = $derived(verdict?.status === "blocked" || verdict?.status === "unchecked");

  let chipTitle = $derived(
    `${harnessPermissionSummary(harness)}${harness?.binary ? `\n${harness.binary}` : ""}\n${modelStatus}`,
  );
  let openManviTitle = $derived(reachable ? manviFocusHint("harness") : unreachableHint);

  let modelTitle = $derived(
    `${modelStatus}${
      discoveryNote && !modelStatus.includes(discoveryNote) ? `\n${discoveryNote}` : ""
    }${preferred ? `\n${preferred.model} at ${preferred.base_url}`
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

{#snippet verdictButton(inline: boolean)}
  {#if verdict}
    <button
      type="button"
      onclick={() => openManvi("activity")}
      disabled={!reachable}
      title={verdictTitle}
      role={inline ? undefined : "menuitem"}
      class={inline
        ? "gp-chip shrink-0 gap-1.5 enabled:cursor-pointer disabled:cursor-default"
        : "gp-menu-item disabled:cursor-default disabled:opacity-50 disabled:hover:bg-transparent"}
      data-tone={inline ? (verdict.status === "blocked" ? "danger" : "unknown") : undefined}
    >
      <ShieldQuestion size={12} />
      <span class="truncate max-w-[150px]">{verdictLabel(verdict)}</span>
    </button>
  {/if}
{/snippet}

{#if $interfaceStore.showHarnessBadges}
<div class="flex items-center gap-1.5" data-testid="manvi-titlebar-status">
  <!-- One chip for Manvi: its tone is the permission mode, so connected,
       unavailable, "a rule fired" and "not probed yet" stay distinct states.
       The model face joins it on wide windows (1440px and up); below 1100px
       only the shield stays, and the accessible name keeps "Manvi" and the
       state. A failed model check keeps its alert icon on the chip. -->
  <button
    bind:this={chipEl}
    type="button"
    onclick={toggleMenu}
    title={chipTitle}
    aria-label={chipLabel}
    aria-haspopup="menu"
    aria-expanded={open}
    data-manvi-chip
    class="gp-chip min-h-6 shrink-0 cursor-pointer gap-1.5"
    data-tone={harnessTone}
  >
    {#if permissionMode === "connected"}
      <ShieldCheck size={12} />
    {:else if permissionMode === "not-probed"}
      <ShieldQuestion size={12} />
    {:else}
      <ShieldAlert size={12} />
    {/if}
    <span class="font-medium max-[1099px]:sr-only">Manvi</span>
    {#if modelAttention}
      <CircleAlert size={12} class="shrink-0 text-danger" aria-hidden="true" />
    {/if}
    <span class="hidden min-w-0 max-w-40 truncate text-textMuted min-[1440px]:inline" aria-hidden="true">{closedFace}</span>
  </button>

  <!-- Screen readers get the model state whether or not the menu is open. -->
  <span id="{instanceId}-model-status" class="sr-only" role="status" aria-live="polite" aria-atomic="true">{modelStatus}</span>

  {#if verdict && verdictInline}
    {@render verdictButton(true)}
  {/if}

  {#if open}
    <div
      bind:this={menuEl}
      use:portal={"body"}
      use:popover={menuPopover} out:popoverExit
      data-manvi-menu
      class="fixed gp-menu gp-pop w-72 shadow-float text-xs"
      style="z-index: {LAYERS.MENU}"
      role="menu"
      aria-label="Manvi and local model"
    >
      <div role="none" class="flex flex-col gap-1.5 p-1.5">
        <p class="gp-label px-1">Local model</p>
        <div role="none" class="flex items-center gap-1.5">
          <!-- Native selection supplies keyboard navigation, typeahead and
               dismissal. The select stays for the model list, but it is drawn
               invisibly over a short face: its closed width follows the face,
               not the longest option, and a long model name ellipsizes. The
               failure sentence is the tooltip. -->
          <div class="relative min-w-0 flex-1 focus-within:outline-2 focus-within:-outline-offset-2 focus-within:outline-accent">
            <span class="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 {modelTone}" aria-hidden="true">
              {#if modelAttention}
                <CircleAlert size={12} />
              {:else}
                <Sparkles size={12} />
              {/if}
            </span>
            <span aria-hidden="true" data-manvi-face={closedFace} class="gp-select block truncate pl-7 pr-5">{closedFace}</span>
            <select
              aria-label="Local model"
              aria-describedby="{instanceId}-model-status"
              aria-busy={$harnessStore.isProbing}
              title={modelTitle}
              value={preferredKey}
              onchange={(event) => pickModel(event.currentTarget.value)}
              class="absolute inset-0 h-full w-full cursor-pointer opacity-0"
            >
              <option value="">{automaticOption}</option>
              {#if preferred && !preferredListed}
                <option value={preferredKey} disabled>{preferred.model} ({$harnessStore.isProbing ? "checking…" : "unavailable"})</option>
              {/if}
              {#each endpoints as endpoint (endpoint.base_url)}
                <optgroup label={endpoint.base_url}>
                  {#each endpoint.models as model (model)}
                    <option value={JSON.stringify([endpoint.base_url, model])}>{model}</option>
                  {/each}
                </optgroup>
              {/each}
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
        <span class="gp-menu-sep" role="separator"></span>
        {#if verdict && !verdictInline}
          {@render verdictButton(false)}
        {/if}
        <button
          type="button"
          role="menuitem"
          onclick={() => openManvi("harness")}
          disabled={!reachable}
          title={openManviTitle}
          class="gp-menu-item disabled:cursor-default disabled:opacity-50 disabled:hover:bg-transparent"
        >
          <ShieldCheck size={12} class="text-textMuted" />
          <span>Open Manvi</span>
        </button>
      </div>
    </div>
  {/if}
</div>
{/if}
