<script lang="ts">
  /**
   * Which model Manvi and GitPulse's own AI features run on: Claude through the
   * Anthropic API (the key lives in `ManviClaudeModel`), or a model server on
   * this machine, which this card discovers, lists and lets the reader pin.
   *
   * Distinct from Agent models above it in Settings → Agents: those are the
   * models an agent CLI starts with in a terminal; these are the models behind
   * Manvi's planning and GitPulse's commit messages and explanations.
   */
  import { harnessStore, type AiSelection } from "../stores/harnessStore";
  import { invoke } from "../ipc/invoke";
  import {
    contextWindowLabel,
    sweepSummary,
    toolSupportLabel,
    type ScanModel,
    type ScanResult,
  } from "../ai/scan";
  import { describeFailure } from "../ui/copy/describeFailure";
  import { manviSectionId } from "../ui/manviFocus";
  import Skeleton from "./Skeleton.svelte";
  import ManviClaudeModel from "./ManviClaudeModel.svelte";
  import { RefreshCw, Server, Check } from "@lucide/svelte";

  let ai = $derived($harnessStore.ai);
  let preferred = $derived($harnessStore.preferred);

  /** "Couldn't <verb> <object>. <Reason>." for an error line. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }

  function isSelected(endpointUrl: string, model: string): boolean {
    if (preferred) return preferred.base_url === endpointUrl && preferred.model === model;
    return ai?.selected?.base_url === endpointUrl && ai?.selected?.model === model;
  }

  /**
   * What the local-server sweep found.
   *
   * The endpoint list reports servers GitPulse already resolved. This answers
   * the question it could not: what each of them actually serves, and whether
   * those models support the features about to be offered on them.
   */
  let scan = $state<ScanResult | null>(null);
  let scanning = $state(false);
  let scanError = $state<string | null>(null);

  async function runScan() {
    scanning = true;
    scanError = null;
    try {
      scan = await invoke<ScanResult>("cmd_local_scan");
    } catch (e) {
      // Reported, not swallowed: a sweep that failed is not a machine with no
      // models on it.
      scanError = failure("scan", "local model servers", e);
      scan = null;
    } finally {
      scanning = false;
    }
  }

  /** Models a chat feature can actually run on, given what the sweep asked. */
  function usable(model: ScanModel): boolean {
    // `capabilities_known` false means nobody asked, so nothing is excluded on
    // the strength of a flag nobody set.
    return !model.capabilities_known || model.supports_completion;
  }

  async function pick(selection: AiSelection | null) {
    await harnessStore.selectModel(selection);
  }
</script>

<div class="space-y-4">
  <ManviClaudeModel />

  <section id={manviSectionId("model")} tabindex="-1" class="gp-card p-4 space-y-2">
    <h3 class="gp-label">Local model servers</h3>
    {#if ai}
      <div class="flex items-center justify-between gap-2">
        <button
          onclick={() => void runScan()}
          disabled={scanning}
          class="gp-btn"
          data-variant="ghost"
          data-size="sm"
          title="Probe the well-known local endpoints and list what each server serves"
        >
          <RefreshCw size={11} class={scanning ? "animate-spin" : ""} />
          <span>{scanning ? "Scanning…" : "Scan local servers"}</span>
        </button>
        {#if scan}
          <span class="text-micro text-textMuted">{sweepSummary(scan)}</span>
        {/if}
      </div>

      {#if scanError}
        <div class="gp-notice" data-tone="unknown" role="status">
          {scanError} This is not a list of what is running.
        </div>
      {:else if scan && scan.servers.length > 0}
        <div class="space-y-1.5">
          {#each scan.servers as server, i (`${server.base_url}#${i}`)}
            <div class="rounded-xl bg-background p-2.5 space-y-1">
              <div class="flex items-center gap-2 text-meta">
                <Server size={12} class="text-accent shrink-0" />
                <span class="font-mono truncate">{server.base_url}</span>
                <span class="gp-chip shrink-0" data-tone="neutral" data-kind="label">{server.runtime}</span>
                {#if server.version}
                  <span class="shrink-0 text-micro text-textMuted">v{server.version}</span>
                {/if}
              </div>
              {#each server.models as model, j (`${model.id || "model"}#${j}`)}
                <button
                  onclick={() => pick({ base_url: server.base_url, model: model.id })}
                  class="gp-row w-full text-left text-micro {usable(model) ? '' : 'opacity-50'}"
                  data-selected={isSelected(server.base_url, model.id)}
                  title={usable(model)
                    ? `${contextWindowLabel(model)} · ${toolSupportLabel(model)}`
                    : "This model does not generate text — an embedding model answers the same listing as every chat model."}
                >
                  <span class="font-mono truncate flex-1">{model.id}</span>
                  <span class="shrink-0 text-textMuted">{contextWindowLabel(model)}</span>
                  <!--
                    Three states, not two. `capabilities_known` false means
                    nobody asked, and rendering that as "no tools" would make a
                    capable model look incapable.
                  -->
                  <span
                    class="gp-chip shrink-0"
                    data-kind="label"
                    data-tone={!model.capabilities_known ? "unknown" : model.supports_tools ? "accent" : "neutral"}
                  >{toolSupportLabel(model)}</span>
                </button>
              {/each}
            </div>
          {/each}
        </div>
      {/if}

      {#if ai.endpoints.filter((e) => e.reachable).length === 0}
        <div class="gp-notice" data-tone="unknown" role="status">
          <div class="min-w-0 flex-1">
            {ai.detail || "No local model server answered."}
            <p class="mt-1.5 text-textMuted">
              GitPulse only ever talks to a model server on this machine: the transport refuses
              any address that is not loopback, so a diff cannot leave the machine through a
              mistyped setting.
            </p>
          </div>
        </div>
      {:else}
        <div class="space-y-2">
          {#each ai.endpoints.filter((e) => e.reachable) as endpoint}
            <div class="rounded-xl bg-background p-3 space-y-2">
              <div class="flex items-center gap-2 text-textPrimary font-medium">
                <Server size={13} class="text-accent" />
                <span class="font-mono text-meta">{endpoint.base_url}</span>
              </div>
              <div class="grid grid-cols-1 gap-1">
                {#each endpoint.models as model}
                  <button
                    onclick={() => pick({ base_url: endpoint.base_url, model })}
                    class="gp-row justify-between text-left"
                    data-selected={isSelected(endpoint.base_url, model)}
                  >
                    <span class="font-mono text-meta truncate">{model}</span>
                    {#if isSelected(endpoint.base_url, model)}
                      <Check size={13} />
                    {/if}
                  </button>
                {/each}
              </div>
            </div>
          {/each}
          {#if preferred}
            <button
              onclick={() => pick(null)}
              class="gp-btn"
              data-variant="ghost"
              data-size="sm"
            >
              Clear the pinned model and let discovery choose
            </button>
          {/if}
        </div>
      {/if}

      {#if ai.model_info}
        <div class="rounded-xl bg-background p-3 space-y-1">
          <div class="text-textPrimary font-medium">{ai.model_info.model}</div>
          <!-- Provenance, not just a number: a window read off the server
               and one typed into a default produce the same request and
               completely different confidence. -->
          <div class="text-textMuted">
            Context: {ai.model_info.describe}
            {#if !ai.model_info.discovered}
              <span class="text-warn"> (declared, not discovered)</span>
            {/if}
          </div>
          {#if ai.model_info.capabilities_known}
            <div class="flex flex-wrap gap-1 pt-1">
              {#if ai.model_info.supports_tools}<span class="gp-chip" data-tone="neutral" data-kind="label">tools</span>{/if}
              {#if ai.model_info.supports_vision}<span class="gp-chip" data-tone="neutral" data-kind="label">vision</span>{/if}
              {#if ai.model_info.supports_reasoning}<span class="gp-chip" data-tone="neutral" data-kind="label">reasoning</span>{/if}
            </div>
          {:else}
            <div class="text-textMuted">The server published no capability list.</div>
          {/if}
        </div>
      {:else if ai.model_detail}
        <div class="gp-notice" data-tone="warn" role="status">{ai.model_detail}</div>
      {/if}
    {:else}
      <Skeleton variant="text" count={2} />
    {/if}
  </section>
</div>
