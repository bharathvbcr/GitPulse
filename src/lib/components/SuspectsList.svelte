<script lang="ts">
  /**
   * Graph's Suspects list: which commits since a known-good ref could have
   * caused a symptom, ranked by what the call graph reaches.
   *
   * One of three lists of commits above the same details pane (see
   * `views/historyList.ts`). The commit graph and HEAD moves read the history
   * forwards; this one reads it backwards, from a misbehaving symbol to the
   * commits that touched what it reaches. The question sits in History's
   * context bar (`SuspectsQueryBar`) and the answer in `suspectsQuery`, so it
   * survives switching lists and the other lists can mark what it ranked.
   *
   * A click selects the commit in place, through the same path as the other
   * two lists, and pins it: the details pane stays on it even when a graph
   * reload stops drawing it. It used to jump to the Diff section, leaving the
   * ranking behind.
   *
   * Two things this list must never let the reader assume:
   *
   *  - The window ends at the *indexed* head, not at HEAD. The blamed lines and
   *    the graph's byte offsets have to describe the same content, so commits
   *    made since the last index are outside the question — stated in the scope
   *    strip rather than left to be discovered.
   *  - An empty list after a real walk ("nothing in the cone changed") and a
   *    refusal ("the walk never ran") are different answers. They are rendered
   *    differently here, and `cone_size` / `blamed_symbols` say how much was
   *    actually examined.
   *
   * `score` is comparable only within one answer. It is rendered as a rank and
   * a bar relative to the top hit, never as a percentage or a probability.
   */
  import { Bug, Loader2 } from "@lucide/svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import {
    suspectsQuery,
    bodyChangedLabel,
    evidenceClass,
    evidenceLabel,
    evidenceTitle,
    hopsLabel,
    relativeScoreWidth,
    topSuspectScore,
  } from "../codeintel/suspectsQuery";
  import { boundedJoin, tooltipWalkIncomplete } from "../codeintel/walkIncomplete";
  import { formatRelativeTime, formatDate, shortHash } from "../format";
  import EmptyState from "./EmptyState.svelte";

  const repoPath = $derived($currentRepoPath);
  // Another repository's answer is not this one's: the store drops it on a
  // switch, and the check here keeps a frame from drawing it in between.
  const answer = $derived($suspectsQuery.repoPath === repoPath ? $suspectsQuery.answer : null);
  const scope = $derived(answer?.scope ?? null);
  const suspects = $derived(answer?.items ?? []);
  const topScore = $derived(topSuspectScore(suspects));
  const selectedCommitId = $derived($graphStore.selectedCommit?.id ?? null);

  function select(commit: string, rank: number) {
    repoStore.selectHistoryCommit(commit, { summary: `Suspect #${rank}`, pin: true });
  }
</script>

<div class="flex-1 min-h-0 overflow-auto px-3 py-2 font-sans" aria-label="Regression suspects">
  <div class="flex flex-col gap-2">
    {#if $suspectsQuery.error}
      <p class="rounded-lg border border-red-400/40 bg-red-400/5 p-2 text-[11px] text-red-400" role="alert">
        {$suspectsQuery.error}
      </p>
    {/if}

    {#if $suspectsQuery.loading}
      <div class="flex items-center gap-1.5 text-[10px] text-textMuted">
        <Loader2 size={11} class="animate-spin" />
        <span>Walking the cone and blaming the lines it reaches…</span>
      </div>
    {/if}

    {#if !answer}
      {#if !$suspectsQuery.loading && !$suspectsQuery.error}
        <EmptyState
          icon={Bug}
          title="Which commit broke it?"
          hint={repoPath
            ? "Name the misbehaving symbol and the last ref it was well at, above. The call graph is walked out from the symptom and the lines it reaches are blamed, so the commits here are ranked by what they touched — not by how recent they are. Any commit's details offer “Find suspects since this commit”."
            : "Open a repository to run this query."}
        />
      {/if}
    {:else}
      {#if scope}
        <div class="flex flex-wrap items-baseline gap-x-3 gap-y-0.5 rounded-lg border border-border/60 bg-background/60 px-2 py-1.5">
          <span class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Examined</span>
          <span class="font-mono text-[10px] text-textSecondary">
            <span title="The commit the code graph was built at. Commits made since it was built are outside this window, because the blamed lines and the graph's offsets have to describe the same content.">
              {shortHash(scope.indexed_head) || "unknown"}
            </span>
            <span class="text-textMuted">…back to…</span>
            <span>{scope.since || "unknown"}</span>
          </span>
          <span class="font-mono text-[10px] tabular-nums text-textMuted">
            {scope.cone_size.toLocaleString()} symbol{scope.cone_size === 1 ? "" : "s"} in the cone ·
            {scope.blamed_symbols.toLocaleString()} blamed
          </span>
          <span class="text-[10px] text-textMuted">The window ends at the indexed commit, not at HEAD.</span>
          {#if scope.refusals.length > 0}
            <span class="text-[10px] text-amber-500" title={boundedJoin(scope.refusals, 12)}>
              {scope.refusals.length} part{scope.refusals.length === 1 ? "" : "s"} of the walk refused
              — the ranking below is a lower bound.
            </span>
          {/if}
        </div>
      {/if}

      {#if !answer.available}
        {@const reasonTitle = tooltipWalkIncomplete([answer.reason])}
        <p class="min-w-0 line-clamp-3 rounded-lg border border-amber-500/40 bg-amber-500/5 p-2 text-[11px] text-amber-500" title={reasonTitle}>
          Suspects unavailable{reasonTitle ? `: ${reasonTitle}` : ""} — not the same as no suspects.
        </p>
      {:else if suspects.length === 0}
        <p class="rounded-lg border border-border/60 bg-background/60 p-2 text-[11px] text-textSecondary">
          Nothing in the cone was touched in this window. The walk ran; it found
          no commit that reaches the symptom.
        </p>
      {:else}
        <div class="flex items-baseline justify-between gap-2">
          <span class="min-w-0 truncate text-[10px] font-bold uppercase tracking-wider text-textMuted" title={answer.symptom}>
            Ranked suspects for <span class="font-mono normal-case">{answer.symptom}</span>
          </span>
          <span class="shrink-0 font-mono text-[10px] tabular-nums text-textMuted">
            {suspects.length} of {answer.total.toLocaleString()}
          </span>
        </div>

        <ul class="flex flex-col gap-1">
          {#each suspects as suspect, index (suspect.commit)}
            {@const selected = selectedCommitId === suspect.commit}
            <li>
              <button
                type="button"
                aria-pressed={selected}
                class="flex w-full flex-col gap-1 rounded-lg border p-2 text-left transition-colors {selected
                  ? 'border-accent/60 bg-accent/10'
                  : 'border-border/60 bg-background/60 hover:border-accent/50 hover:bg-surface/60'}"
                onclick={() => select(suspect.commit, index + 1)}
                title="Show this commit in the details below"
              >
                <div class="flex items-center gap-2">
                  <span class="w-5 shrink-0 font-mono text-[10px] tabular-nums text-textMuted">
                    #{index + 1}
                  </span>
                  <span class="font-mono text-[11px] text-textPrimary">{shortHash(suspect.commit)}</span>
                  <span
                    class="shrink-0 rounded border px-1 text-[9px] uppercase tracking-wide {evidenceClass(suspect.evidence)}"
                    title={evidenceTitle(suspect.evidence)}
                  >
                    {evidenceLabel(suspect.evidence)}
                  </span>
                  <span class="min-w-0 flex-1 truncate text-[10px] text-textMuted">
                    {suspect.author}
                  </span>
                  <span class="shrink-0 text-[10px] text-textMuted" title={formatDate(suspect.author_time)}>
                    {formatRelativeTime(suspect.author_time)}
                  </span>
                </div>

                <div class="flex items-center gap-2">
                  <!-- Relative to the top hit, never a percentage: the score is
                       comparable only against other scores in this answer. -->
                  <div class="h-1 min-w-0 flex-1 overflow-hidden rounded-full bg-border/40">
                    <div
                      class="h-full rounded-full bg-accent/70"
                      style:width="{relativeScoreWidth(suspect.score, topScore)}%"
                    ></div>
                  </div>
                  <span
                    class="shrink-0 font-mono text-[10px] tabular-nums text-textMuted"
                    title="Call edges from the symptom to the nearest symbol this commit touched. Zero means it touched the symptom itself."
                  >
                    {hopsLabel(suspect.nearest_distance)}
                  </span>
                </div>

                {#if suspect.touched.length > 0}
                  <ul class="flex flex-col gap-0.5 pl-7">
                    {#each suspect.touched.slice(0, 4) as touch (touch.qualified_name + touch.file_path)}
                      <li class="flex items-baseline gap-2 font-mono text-[10px] text-textSecondary">
                        <span class="min-w-0 truncate" title={touch.file_path}>
                          {touch.qualified_name}
                        </span>
                        <span class="shrink-0 tabular-nums text-textMuted">
                          {touch.distance} hop · {touch.lines} line{touch.lines === 1 ? "" : "s"}
                        </span>
                        <span
                          class="shrink-0 {touch.body_changed === null ? 'text-amber-500' : 'text-textMuted'}"
                          title={touch.body_changed === null
                            ? "Whether the body changed could not be established — not the same as unchanged."
                            : "Whether this commit changed the symbol's body or only moved it."}
                        >
                          {bodyChangedLabel(touch.body_changed)}
                        </span>
                      </li>
                    {/each}
                    {#if suspect.touched.length > 4}
                      <li
                        class="font-mono text-[10px] text-textMuted"
                        title={boundedJoin(suspect.touched.slice(4).map((t) => t.qualified_name), 12)}
                      >
                        +{suspect.touched.length - 4} more symbol{suspect.touched.length - 4 === 1 ? "" : "s"}
                      </li>
                    {/if}
                  </ul>
                {/if}
              </button>
            </li>
          {/each}
        </ul>

        {#if answer.truncated}
          <p class="text-[10px] text-amber-500">
            Suspect list truncated by token budget — commits below the cut are
            not ruled out.
          </p>
        {/if}
      {/if}

      {#if answer.walkIncomplete}
        {@const walkTitle = tooltipWalkIncomplete([answer.walkIncomplete])}
        <p class="min-w-0 line-clamp-3 text-[10px] text-amber-500" title={walkTitle}>
          Walk incomplete: {answer.walkIncomplete}
        </p>
      {/if}
    {/if}
  </div>
</div>
