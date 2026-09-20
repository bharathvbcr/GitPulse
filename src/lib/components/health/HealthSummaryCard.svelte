<script lang="ts">
  // `ShieldQuestionMark`, not the `ShieldQuestion` alias: that one is
  // deprecated upstream and resolves through the aliases barrel.
  import { ShieldAlert, ShieldCheck, ShieldQuestionMark } from "@lucide/svelte";
  import { toneCardClass, toneChipClass, toneLabel } from "../../health/format";
  import { healthSectionDomId } from "../../health/sections";
  import type { HealthSummary } from "../../health/summary";

  /**
   * The verdict, at the top of the scroll.
   *
   * The panel had nothing of the kind. A reader arrived at a page that opened
   * with a paragraph about GitHub CLI permissions, then an inventory of
   * package manifests, and reached the repository's one high-severity
   * vulnerability roughly a thousand pixels down. The header's attempt at a
   * summary was five `truncate` spans in one flex row, which is why it
   * rendered as "58 outd… · Dependabot 0 … · Code scanning unav…".
   *
   * Two jobs, one control: each facet is both a line of the verdict and the
   * jump to the section it summarises, so the page gains navigation without
   * spending a second row of chrome on it.
   *
   * What it will not do is claim an all-clear it cannot support.
   * `summary.clearClaimable` is false the moment any facet is unestablished,
   * and the caveat list below names every one of them in the scanner's own
   * words. A check that could not run must never read like one that ran and
   * passed.
   */
  let {
    summary,
    /**
     * Section ids actually present in the DOM. A facet whose section did not
     * render (no code graph, so no dead-code table) still states its verdict,
     * but as text rather than as a control that would scroll nowhere.
     */
    rendered,
  }: {
    summary: HealthSummary;
    rendered: ReadonlySet<string>;
  } = $props();

  function jump(id: string) {
    const target = document.getElementById(healthSectionDomId(id));
    if (!target) return;
    target.scrollIntoView({ behavior: "smooth", block: "start" });
    // Move focus as well as the viewport: a scroll that keyboard focus does
    // not follow leaves the next Tab back at the top of the page.
    target.focus({ preventScroll: true });
  }
</script>

<div
  class="rounded-2xl border p-4 space-y-3 shadow-card {toneCardClass(summary.tone)}"
>
  <div class="flex items-start gap-2.5">
    {#if summary.tone === "clear"}
      <ShieldCheck size={18} class="text-emerald-300 shrink-0 mt-px" aria-hidden="true" />
    {:else if summary.tone === "unknown"}
      <ShieldQuestionMark size={18} class="text-amber-300 shrink-0 mt-px" aria-hidden="true" />
    {:else}
      <ShieldAlert
        size={18}
        class="{summary.tone === 'critical' ? 'text-rose-300' : 'text-amber-300'} shrink-0 mt-px"
        aria-hidden="true"
      />
    {/if}
    <p class="text-[13px] leading-snug text-textPrimary font-medium">
      {summary.headline}
    </p>
  </div>

  <ul class="flex flex-wrap gap-1.5" aria-label="Health summary by area">
    {#each summary.facets as facet (facet.id)}
      <li>
        {#if rendered.has(facet.id)}
          <button
            type="button"
            onclick={() => jump(facet.id)}
            class="px-2 py-1 rounded-full border text-[11px] hover:brightness-125 transition focus-visible:outline-2 focus-visible:outline-accent {toneChipClass(
              facet.tone,
            )}"
            title={facet.caveat ?? `${facet.label}: ${facet.value}`}
          >
            <span class="font-medium">{facet.label}</span>
            <span class="opacity-80">{facet.value}</span>
            <span class="sr-only">— {toneLabel(facet.tone)}. Jump to section.</span>
          </button>
        {:else}
          <span
            class="px-2 py-1 rounded-full border text-[11px] {toneChipClass(facet.tone)}"
            title={facet.caveat ?? `${facet.label}: ${facet.value}`}
          >
            <span class="font-medium">{facet.label}</span>
            <span class="opacity-80">{facet.value}</span>
            <span class="sr-only">— {toneLabel(facet.tone)}.</span>
          </span>
        {/if}
      </li>
    {/each}
  </ul>

  {#if summary.caveats.length > 0}
    <details class="group">
      <summary
        class="cursor-pointer text-[11px] text-amber-300 marker:content-[''] select-none"
      >
        <span class="group-open:hidden">
          {summary.caveats.length}
          {summary.caveats.length === 1 ? "reason" : "reasons"} this is not a complete
          all-clear — show
        </span>
        <span class="hidden group-open:inline">Hide what this scan could not establish</span>
      </summary>
      <ul class="mt-2 space-y-1 text-[11px] text-textSecondary list-disc pl-4">
        {#each summary.caveats as caveat, index (`${index}:${caveat}`)}
          <li class="leading-relaxed">{caveat}</li>
        {/each}
      </ul>
    </details>
  {:else}
    <p class="text-[11px] text-emerald-300">
      Every scanner this repository supports ran to completion.
    </p>
  {/if}
</div>
