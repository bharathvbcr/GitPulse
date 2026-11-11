<script lang="ts">
  /**
   * Row 2 of the header: the lenses on the selected repository
   * (HEADER_SPEC section 3).
   *
   * Row 1 says which repository or workspace surface you are on; this row says
   * how you are looking at it, and what state it is in. Left to right: the view
   * switcher, the view's own toolbar when the row is wide enough to hold it
   * (`ViewContextBar` hands its node to the slot below), then a drag zone, then
   * the trailing cluster: this repository's Agents pill, the Terminal pill, the
   * uncommitted chip and Refresh. The two "come here" controls sit nearest the
   * content; the passive state sits at the edge.
   *
   * Under Fleet, Agents or Tasks the row stays mounted at the same height with
   * the controls `inert` and hidden, and the surface's name in their place, so
   * the workspace below never jumps. Widths are container queries on this
   * element, not the window, so a narrow stage behaves like a narrow window.
   */
  import { onMount, untrack } from "svelte";
  import { fade } from "svelte/transition";
  import { Bot, FileDiff, RefreshCw } from "@lucide/svelte";
  import { repoStore } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { headerAgentPlane } from "../agents/headerPlane";
  import { plural } from "../agents/plane";
  import { setAgentsRepositoryScope } from "../agents/scope";
  import { ensurePlaneRead, repoAgentPill } from "../agents/repoAttention";
  import { agentPlaneStore, sweepTargets } from "../agents/store";
  import { LAYERS } from "../ui/layers";
  import { backdropFade, backdropFadeOut } from "../ui/transitions";
  import { swapRow } from "../ui/headerMotion";
  import { registerContextSlot, reportViewBarWidth } from "../ui/headerSlots";
  import ScrollCue from "./ScrollCue.svelte";
  import PageSearch from "./PageSearch.svelte";
  import TerminalPill from "./TerminalPill.svelte";
  import ViewTabBar from "./ViewTabBar.svelte";

  let { conflictedCount = 0 }: { conflictedCount?: number } = $props();

  const SURFACE_LABEL = { fleet: "Fleet", agents: "Agents", tasks: "Tasks" } as const;
  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };

  let bar: HTMLElement | undefined = $state();
  let scroller: HTMLDivElement | undefined = $state();
  let slot: HTMLDivElement | undefined = $state();

  const surface = $derived($interfaceStore.globalSurface);
  const onRepository = $derived(surface === "repository");
  const surfaceLabel = $derived(surface === "repository" ? "" : SURFACE_LABEL[surface]);

  const repoPath = $derived($repoStore.currentPath ?? "");
  const agents = $derived(
    repoAgentPill({
      plane: $headerAgentPlane.plane,
      probes: $headerAgentPlane.probes,
      repoPath,
      paths: pathOpts,
      scanning: $agentPlaneStore.scanning,
    }),
  );

  // Row 2 mounting on a repository the Agents read has not covered asks the
  // shared sweep for it (once per repository). Nothing here polls: after this
  // the plane is read when the Agents surface asks, as before.
  const readTargets = $derived(sweepTargets($repoStore.openTabs));
  $effect(() => {
    const path = repoPath;
    const state = $agentPlaneStore;
    if (!path || !onRepository) return;
    const targets = untrack(() => readTargets);
    untrack(() => {
      ensurePlaneRead({ store: agentPlaneStore, state, repoPath: path, targets, paths: pathOpts });
    });
  });
  const agentsLabel = $derived.by(() => {
    if (agents.state === "unknown") return `Agents, ${agents.phrase.toLowerCase()}. ${agents.why} Open Agents for this repository`;
    const live = agents.live > 0 ? `, ${plural(agents.live, "agent running", "agents running")}` : "";
    return agents.state === "attention"
      ? `Agents, ${agents.phrase.toLowerCase()}${live}`
      : `Agents${live}`;
  });

  // Row 2's controls fade (opacity only) when a global surface takes over and
  // when the repository comes back; `concealed` is the final `visibility`, set
  // after the fade out and before the fade in, so the row is never half-hidden.
  let controls: HTMLDivElement | undefined = $state();
  let concealed = $state(!untrack(() => onRepository));
  let swapped: boolean | null = null;
  let swapping: Animation | null = null;
  $effect(() => {
    const show = onRepository;
    const el = controls;
    if (!el || swapped === show) return;
    const first = swapped === null;
    swapped = show;
    if (first) return;
    // A swap interrupted by the next one settles first, so the last word is the newest.
    swapping?.cancel();
    if (show) concealed = false;
    swapping = swapRow(el, show, () => {
      concealed = !show;
    });
  });

  const dirty = $derived($repoStore.statuses.length);

  function openAgents() {
    setAgentsRepositoryScope(repoPath || null);
    interfaceStore.setAgentsOpen(true);
  }

  onMount(() => {
    const dispose = slot ? registerContextSlot(slot) : () => {};
    let observer: ResizeObserver | undefined;
    if (bar && typeof ResizeObserver === "function") {
      observer = new ResizeObserver((entries) => {
        const width = entries[0]?.contentRect.width;
        if (typeof width === "number") reportViewBarWidth(width);
      });
      observer.observe(bar);
    }
    return () => {
      observer?.disconnect();
      dispose();
    };
  });
</script>

<!-- Justified: the bare drag-region attribute starts a window drag only on a
     direct hit, so the controls inside keep their own clicks. -->
<nav
  bind:this={bar}
  aria-label="Views"
  data-viewbar
  data-tauri-drag-region
  class="@container gp-glass gp-repo-tabs relative flex h-9 shrink-0 items-center text-xs select-none"
  style:z-index={LAYERS.STICKY}
>
  {#if !onRepository}
    <span
      class="absolute left-3 top-1/2 -translate-y-1/2 text-sm font-medium text-textPrimary"
      data-viewbar-surface={surface}
      in:fade={backdropFade()}
      out:fade={backdropFadeOut()}
    >{surfaceLabel}</span>
  {/if}

  <div
    bind:this={controls}
    class="flex h-full min-w-0 flex-1 items-center gap-2 px-2 {concealed ? 'invisible' : ''}"
    inert={!onRepository}
    data-viewbar-controls
  >
    <div class="relative h-full min-w-0 shrink">
      <div bind:this={scroller} class="gp-header-scroll h-full min-w-0">
        <!-- Everything in this row is measured as overflow, so nothing may sit
             after the last control: the drag spacer is outside the scroller,
             or the overflow cue would land over empty space. -->
        <div class="flex h-full w-max min-w-full items-center gap-1.5" data-tauri-drag-region>
          <ViewTabBar {conflictedCount} />
        </div>
      </div>
      <ScrollCue target={scroller} axis="x" diffuse />
    </div>

    <!-- The view's own toolbar lands here from 1600px (ViewContextBar):
         History's list segment, Work's conflict count. -->
    <div
      bind:this={slot}
      class="flex min-w-0 items-center gap-3 empty:hidden"
      data-viewbar-context
      data-tauri-drag-region
    ></div>

    <!-- The one search field, for whatever page is on screen
         (search/pageSearch.svelte.ts). It takes three shares of the free
         width to the drag spacer's one; the spacer keeps a 48px floor so the
         window can always be grabbed. -->
    <PageSearch />

    <div class="h-full min-w-12 flex-1" data-tauri-drag-region></div>

    {#if agents.state !== "none"}
      <button
        type="button"
        class="gp-chip shrink-0 cursor-pointer"
        data-tone={agents.tone ?? (agents.state === "unknown" ? "unknown" : "accent")}
        data-testid="viewbar-agents"
        data-agents-state={agents.state}
        aria-label={agentsLabel}
        title={agentsLabel}
        onclick={openAgents}
      >
        <Bot size={12} aria-hidden="true" />
        <span class="hidden @min-[1100px]:inline">{agents.state === "attention" || agents.state === "unknown" ? agents.phrase : "Agents"}</span>
        {#if agents.state === "unknown"}
          <!-- Narrow rows keep the icon: the "?" stands for the word, and the
               tooltip carries the cause. -->
          <span class="@min-[1100px]:hidden" aria-hidden="true">?</span>
        {:else if agents.live > 0}
          <span class="font-mono tabular-nums" aria-hidden="true">{agents.live}</span>
        {/if}
      </button>
    {/if}

    <TerminalPill />

    {#if dirty > 0}
      <button
        type="button"
        class="gp-chip shrink-0 cursor-pointer"
        data-tone="warn"
        data-testid="viewbar-uncommitted"
        title="Preview uncommitted files and modifications"
        aria-label="Preview {dirty} uncommitted changes"
        onclick={() => void repoStore.previewUncommitted()}
      >
        <FileDiff size={12} aria-hidden="true" />
        <span class="font-mono tabular-nums" aria-hidden="true">{dirty}</span>
        <span class="hidden @min-[1100px]:inline" aria-hidden="true">uncommitted</span>
      </button>
    {/if}

    <button
      type="button"
      class="gp-icon-btn shrink-0"
      data-testid="viewbar-refresh"
      title="Refresh status"
      aria-label="Refresh status"
      onclick={() => repoStore.refresh()}
    >
      <RefreshCw size={14} class={$repoStore.isLoading ? "animate-spin motion-reduce:animate-none" : ""} aria-hidden="true" />
    </button>
  </div>
</nav>
