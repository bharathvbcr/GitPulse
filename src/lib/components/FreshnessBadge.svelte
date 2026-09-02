<script lang="ts">
  import { freshnessBadge, isNoteworthy, type FreshnessTone } from "../provenance/badge";
  import type { ProvenanceFreshness } from "../provenance/types";

  let {
    freshness,
    compact = false,
  }: {
    /** Null while the measurement is still in flight, or was never asked for. */
    freshness: ProvenanceFreshness | null;
    /** Dot-only, for dense rows like the branch list. */
    compact?: boolean;
  } = $props();

  const badge = $derived(freshness ? freshnessBadge(freshness) : null);

  /**
   * Tone is a property of the claim, never of the kind's name: `unknown` and
   * `unverified` are both muted because neither is an accusation, and `stale`
   * is amber because a decayed verification still is one.
   */
  const DOT: Record<FreshnessTone, string> = {
    good: "bg-emerald-500/80",
    warn: "bg-amber-500",
    bad: "bg-rose-500",
    muted: "bg-muted-foreground/40",
  };

  const PILL: Record<FreshnessTone, string> = {
    good: "text-emerald-600 dark:text-emerald-400 bg-emerald-500/10",
    warn: "text-amber-600 dark:text-amber-400 bg-amber-500/10",
    bad: "text-rose-600 dark:text-rose-400 bg-rose-500/10",
    muted: "text-muted-foreground bg-muted-foreground/10",
  };
</script>

<!-- Nothing is drawn for a commit we know carries no provenance. In a
     repository that has only just started recording, that is nearly every row,
     and a badge on every row makes the exceptions invisible — the same reason
     BranchHealthDot draws only what needs attention. Every other state,
     including "we could not tell", is worth seeing. -->
{#if badge && isNoteworthy(badge)}
  {#if compact}
    <span
      class="w-1.5 h-1.5 rounded-full shrink-0 {DOT[badge.tone]}"
      role="img"
      aria-label="{badge.label}: {badge.detail}"
      title="{badge.label} — {badge.detail}"
      data-testid="freshness-dot"
      data-kind={badge.kind}
    ></span>
  {:else}
    <span
      class="px-1.5 py-0.5 rounded text-[10px] font-medium shrink-0 whitespace-nowrap {PILL[
        badge.tone
      ]}"
      title={badge.detail}
      data-testid="freshness-badge"
      data-kind={badge.kind}
    >
      {badge.label}
    </span>
  {/if}
{/if}
