<script lang="ts" module>
  /**
   * The collapsed strip, in two pieces.
   *
   * The full sentences are long by design (they name the namespaces and the
   * way out), and stacking several of them above the graph would cost more
   * rows than the history they describe. So the strip leads with the first
   * notice and the rest are reachable by expanding.
   *
   * `overflow` is returned separately rather than appended because the lead
   * is rendered in a truncating box: glued on the end, "+1 more" is the first
   * thing the ellipsis eats, and the reader is told there is one notice when
   * there are three.
   */
  export function noticeSummary(notices: readonly string[]): {
    lead: string;
    overflow: string;
  } {
    return {
      lead: notices[0] ?? "",
      overflow: notices.length > 1 ? `+${notices.length - 1} more` : "",
    };
  }
</script>

<script lang="ts">
  import { EyeOff, ChevronDown, ChevronRight } from "lucide-svelte";

  /**
   * Completeness disclosures for a healthy graph load: history outside the
   * walked ref scope, a capped label set.
   *
   * These used to have no renderer at all. `graphStore` carried them in
   * `state.warnings`, nothing read that field, and the only surface they ever
   * reached was the diagnostics ring — the app's crash log. A true statement
   * about the repository ("36 commits live in refs/cmux and this scope does
   * not draw them") therefore arrived looking like a malfunction, once per
   * repository per launch, and the remedy it names lives in Settings where
   * the log cannot link to it.
   *
   * Faults still go to diagnostics; see `GraphNotes` in the backend for the
   * rule that decides which list an entry belongs in.
   */
  let { notices }: { notices: readonly string[] } = $props();

  let expanded = $state(false);
  const summary = $derived(noticeSummary(notices));
  const many = $derived(notices.length > 1);
</script>

{#if notices.length > 0}
  <div
    class="shrink-0 border-b border-border gp-section-edge bg-surface/60 px-3 py-1 text-[11px] text-textMuted"
    data-testid="graph-notices"
  >
    <div class="flex items-center gap-1.5">
      <EyeOff size={11} class="shrink-0 opacity-70" />
      {#if many}
        <button
          class="shrink-0 opacity-70 hover:opacity-100"
          aria-expanded={expanded}
          aria-label={expanded ? "Collapse notices" : "Expand notices"}
          onclick={() => (expanded = !expanded)}
        >
          {#if expanded}
            <ChevronDown size={11} />
          {:else}
            <ChevronRight size={11} />
          {/if}
        </button>
      {/if}
      <span class="min-w-0 flex-1 truncate" title={notices.join("\n\n")}>
        {expanded ? "Not drawn in this graph:" : summary.lead}
      </span>
      {#if !expanded && summary.overflow}
        <span class="shrink-0 opacity-80">{summary.overflow}</span>
      {/if}
      <button
        class="shrink-0 underline decoration-dotted underline-offset-2 hover:text-textPrimary"
        onclick={() => window.dispatchEvent(new CustomEvent("gitpulse:settings"))}
      >
        Settings
      </button>
    </div>
    {#if expanded}
      <ul class="mt-1 space-y-0.5 pl-[26px]">
        {#each notices as notice (notice)}
          <li class="break-words">{notice}</li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}
