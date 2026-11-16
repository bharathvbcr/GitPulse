<script lang="ts">
  /**
   * The one inline notice for a surface that reads the DevCouncil code map when
   * the map cannot be read: what is missing, what setting it up gives this
   * surface, and the button that fixes it.
   *
   *  - CLI not installed: "Set up code map" opens the setup wizard.
   *  - CLI installed, no index: "Build index" runs the same build the Map panel
   *    runs (`buildDevmap`, blocked while the automatic index is running).
   *  - Anything else (the graph answers, its status could not be read, or the
   *    read is still out): no install prompt. `otherText` is the surface's own
   *    honest failure text, shown meanwhile too; a surface with none renders
   *    nothing.
   *
   * The cause comes from `classifyGraphGap`, i.e. a status read plus
   * `onboardingStore.status`, never from the wording of a reply's reason. A
   * surface that already holds the status passes it as `graph`; otherwise the
   * notice reads it itself, once per repository.
   */
  import { Loader2 } from "@lucide/svelte";
  import { buildBlockReason } from "../codeintel/buildBlock";
  import { liveIndex } from "../codeintel/liveIndex";
  import { tooltipWalkIncomplete } from "../codeintel/walkIncomplete";
  import {
    buildGraphIndex,
    classifyGraphGap,
    forgetGraphReading,
    readGraphReadingShared,
    type GraphReading,
  } from "../tools/graphGap";
  import { ensureToolsStatus, onboardingStore, openSetupWizard } from "../tools/onboardingStore";

  let {
    repoPath,
    feature,
    graph = null,
    reason = null,
    otherText = null,
    inset = false,
    onBeforeSetup,
    onBuilt,
  }: {
    repoPath: string | null;
    /** What the code map gives this surface, e.g. "impact edges and symbol search". */
    feature: string;
    /** The status the surface already read. Omit to have the notice read it. */
    graph?: { available: boolean } | null;
    /** The reply's raw reason, kept as a tooltip rather than printed. */
    reason?: string | null;
    /** The surface's own failure text when the cause is neither a missing CLI nor a missing index. */
    otherText?: string | null;
    /** Sit inside a bordered card (margins on both sides and below) instead of flush. */
    inset?: boolean;
    /** Runs before the wizard opens, e.g. to close a dialog that would sit over it. */
    onBeforeSetup?: () => void | Promise<void>;
    /** Runs after a build that succeeded, so the surface can reload. */
    onBuilt?: () => void;
  } = $props();

  const toolsStatus = onboardingStore.status;
  const snapshots = liveIndex.snapshots;

  let probed = $state<{ repo: string; reading: GraphReading } | null>(null);
  let building = $state(false);
  let buildError = $state<string | null>(null);

  const known = $derived<GraphReading | null>(
    graph ? (graph.available ? "available" : "unavailable") : null,
  );
  const reading = $derived<GraphReading | null>(
    known ?? (probed && probed.repo === repoPath ? probed.reading : null),
  );
  const gap = $derived(
    reading === null
      ? null
      : classifyGraphGap({ graph: reading, devmapInstalled: $toolsStatus?.devmap.installed ?? null }),
  );
  const blockedReason = $derived(
    buildBlockReason(false, repoPath ? $snapshots[repoPath]?.phase : undefined),
  );
  const reasonTitle = $derived(tooltipWalkIncomplete([reason]) ?? undefined);

  async function probe(repo: string) {
    const result = await readGraphReadingShared(repo);
    if (repo === repoPath) probed = { repo, reading: result };
  }

  $effect(() => {
    const repo = repoPath;
    if (repo && graph === null) void probe(repo);
  });

  // Whether the CLI is installed only decides between the two remedies, so it
  // is read only when the graph is actually unavailable, and once for every
  // notice (a failed read is not retried from here).
  $effect(() => {
    if (reading === "unavailable" && $toolsStatus === null) void ensureToolsStatus();
  });

  async function setup() {
    await onBeforeSetup?.();
    openSetupWizard("devmap", "explain");
  }

  async function build() {
    const repo = repoPath;
    if (!repo || building) return;
    building = true;
    buildError = null;
    try {
      const result = await buildGraphIndex(repo);
      if (repo !== repoPath) return;
      if (!result.ok) {
        buildError = result.error;
        return;
      }
      forgetGraphReading(repo);
      if (graph === null) await probe(repo);
      onBuilt?.();
    } finally {
      building = false;
    }
  }
</script>

{#if gap === "cli_missing" || gap === "no_index"}
  <div
    class="gp-notice flex flex-wrap items-center gap-2 {inset ? 'mx-2.5 mt-0 mb-2' : 'mx-0'}"
    data-tone="warn"
    data-testid="missing-tool-notice"
    data-gap={gap}
    role="status"
    title={reasonTitle}
  >
    <div class="min-w-0 flex-1">
      <p class="text-textPrimary">
        {gap === "cli_missing"
          ? "The code map tool is not installed."
          : "This repository has no code map yet."}
      </p>
      <p class="text-meta text-textMuted">
        {gap === "cli_missing" ? "Set it up" : "Build the index"} to get {feature}.
      </p>
      {#if buildError}
        <p class="text-meta text-danger" role="alert">{buildError}</p>
      {/if}
    </div>
    {#if gap === "cli_missing"}
      <button type="button" class="gp-btn shrink-0 text-meta px-2 py-0.5" onclick={() => void setup()}>
        Set up code map
      </button>
    {:else}
      <button
        type="button"
        class="gp-btn shrink-0 text-meta px-2 py-0.5"
        disabled={building || !repoPath || blockedReason != null}
        title={blockedReason ?? "Build the code map index"}
        aria-busy={building}
        onclick={() => void build()}
      >
        {#if building}
          <Loader2 size={12} class="animate-spin inline-block align-[-2px]" aria-hidden="true" /> Building…
        {:else}
          Build index
        {/if}
      </button>
    {/if}
  </div>
{:else if (gap === "other" || gap === null) && otherText}
  <p class="gp-notice mx-0 mt-0 min-w-0" data-tone="warn" data-testid="missing-tool-other" title={reasonTitle}>
    <span class="min-w-0 line-clamp-3">{otherText}</span>
  </p>
{/if}
