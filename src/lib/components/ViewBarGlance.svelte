<script lang="ts">
  /**
   * Row 2's glance (HEADER_SPEC §3 row 2b, decision D8): files changed in more than one
   * worktree, the branch's pull request and its checks, the last fetch, and
   * the stash. Every fact comes from a read something else already made
   * (ui/viewBarGlance.ts says which), so mounting this asks the host nothing.
   * A chip with nothing to say is absent, never a zero.
   *
   * The glance gives way before the view switcher does. Its strip shrinks
   * first (its flex-shrink outweighs the switcher's a thousandfold), and a
   * chip that no longer fits wraps onto the strip's clipped second line and is
   * hidden whole and taken out of the tab order: the last chip goes first, so
   * the order above is the priority. Work's switcher is the longest, and at
   * the default 1280px window it leaves no room at all.
   */
  import { onMount } from "svelte";
  import { Archive, CloudDownload, GitPullRequest, Layers } from "@lucide/svelte";
  import { repoStore } from "../stores/repoStore";
  import { headerAgentPlane } from "../agents/headerPlane";
  import { probeFor } from "../agents/repoAttention";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { pullRequestReads } from "../github/pullRequestReads";
  import {
    branchPullRequest,
    fetchGlance,
    overlapGlance,
    pullRequestGlance,
    stashGlance,
  } from "../ui/viewBarGlance";

  let { onOpenAgents }: { onOpenAgents: () => void } = $props();

  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };

  // Ages are read against a clock that ticks once a minute: the chips say
  // minutes at their finest, and a faster tick would only rewrite the row.
  let now = $state(Date.now());

  const repoPath = $derived($repoStore.currentPath ?? "");
  const overlap = $derived(
    overlapGlance(repoPath ? probeFor($headerAgentPlane.probes, repoPath, pathOpts)?.snapshot?.collisions : null),
  );
  const pullRequest = $derived.by(() => {
    const read = repoPath ? $pullRequestReads.get(repoPath) : undefined;
    if (!read) return null;
    return pullRequestGlance(branchPullRequest(read.prs, $repoStore.currentBranch), read.at, now);
  });
  // Before a repository's metadata is read its stash is empty and its fetch
  // null (repos/projection.ts), so neither chip shows until there is a read.
  const fetched = $derived(fetchGlance($repoStore.fetchedAt, now));
  const stash = $derived(
    stashGlance({
      entries: $repoStore.stashEntries.length,
      truncated: $repoStore.stashTruncated,
      failed: $repoStore.stashFailed,
    }),
  );

  let strip: HTMLDivElement | undefined = $state();

  /** Hides, whole, every chip the strip's width does not hold. */
  function fit() {
    const el = strip;
    if (!el) return;
    const box = el.getBoundingClientRect();
    for (const chip of el.children) {
      if (!(chip instanceof HTMLElement)) continue;
      const at = chip.getBoundingClientRect();
      const out = at.top >= box.bottom - 0.5 || at.right > box.right + 0.5;
      chip.style.visibility = out ? "hidden" : "";
      chip.inert = out;
      chip.toggleAttribute("data-glance-hidden", out);
    }
  }

  // A chip coming or going can change what fits without the strip changing
  // size (it is already at its share), so the fit is redone after each change
  // to what the chips say. It writes only the chips' own style, never state.
  $effect(() => {
    void [overlap, pullRequest, fetched, stash];
    fit();
  });

  onMount(() => {
    const timer = setInterval(() => (now = Date.now()), 60_000);
    let observer: ResizeObserver | undefined;
    if (strip && typeof ResizeObserver === "function") {
      observer = new ResizeObserver(() => fit());
      observer.observe(strip);
    }
    return () => {
      clearInterval(timer);
      observer?.disconnect();
    };
  });
</script>

<!-- Justified: the strip is a drag region where no chip covers it, like the
     row around it. With no chip it is not displayed, so it costs the row no
     gap either. -->
<div
  bind:this={strip}
  class="flex h-5 min-w-0 shrink-[1000] flex-wrap items-center gap-x-2 overflow-hidden"
  class:hidden={!overlap && !pullRequest && !fetched && !stash}
  data-glance-strip
  data-tauri-drag-region
>
  {#if overlap}
    <button
      type="button"
      class="gp-chip shrink-0 cursor-pointer"
      data-tone={overlap.tone}
      data-glance="overlap"
      aria-label={overlap.label}
      title={overlap.label}
      onclick={onOpenAgents}
    >
      <Layers size={12} aria-hidden="true" />
      <span class="font-mono tabular-nums" aria-hidden="true">{overlap.text}</span>
    </button>
  {/if}

  {#if pullRequest}
    <button
      type="button"
      class="gp-chip shrink-0 cursor-pointer"
      data-tone={pullRequest.tone}
      data-glance="pull-request"
      aria-label="{pullRequest.label}. Open in Work"
      title={pullRequest.label}
      onclick={() => repoStore.setActiveTab("work", "remote")}
    >
      <GitPullRequest size={12} aria-hidden="true" />
      <span class="font-mono tabular-nums" aria-hidden="true">{pullRequest.text}</span>
    </button>
  {/if}

  {#if fetched}
    <button
      type="button"
      class="gp-chip shrink-0 cursor-pointer"
      data-tone={fetched.tone}
      data-glance="fetch"
      aria-label={fetched.label}
      title={fetched.label}
      onclick={() => void repoStore.fetch()}
    >
      <CloudDownload size={12} aria-hidden="true" />
      <span class="font-mono tabular-nums" aria-hidden="true">{fetched.text}</span>
    </button>
  {/if}

  {#if stash}
    <span class="gp-chip shrink-0" data-tone={stash.tone} data-glance="stash" title={stash.label}>
      <Archive size={12} aria-hidden="true" />
      <span class="font-mono tabular-nums" aria-hidden="true">{stash.text}</span>
      <span class="sr-only">{stash.label}</span>
    </span>
  {/if}
</div>
