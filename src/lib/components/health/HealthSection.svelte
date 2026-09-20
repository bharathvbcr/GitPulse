<script lang="ts">
  import {
    HEALTH_CONTENT_WIDTH,
    healthSection,
    healthSectionDomId,
    healthSectionHeadingId,
  } from "../../health/sections";
  import { formatSectionCount, isBounded, type SectionCount } from "../../health/counts";
  import { toneChipClass, toneLabel } from "../../health/format";
  import type { HealthTone } from "../../health/summary";

  /**
   * One section of the Health view.
   *
   * Every section used to hand-roll its own `<section>`, `<h3>`, width and
   * count sentence. That is why the widths stepped between `2xl` and `5xl`
   * down the page, why some headings carried a count and others did not, and
   * why three separate sections each had to remember, on their own, to say
   * that their list had been cut short.
   *
   * The disclosure is the point. `count` is a `SectionCount`, not a
   * pre-rendered string, so there is no path through this component that
   * prints the number of surviving rows without also printing what the scan
   * actually observed. A section added later cannot reintroduce the bug by
   * writing a heading that merely looks compliant.
   */
  let {
    /** Catalog id. The label, heading, description and DOM ids come from it. */
    id,
    count = null,
    /** Verdict tone for this section, shown as a chip beside the heading. */
    tone = null,
    /**
     * Why this section's result is not complete — the scanner's own words.
     * Rendered under the heading, above the content, so it cannot be scrolled
     * past on the way to a table that looks authoritative.
     */
    caveat = null,
    /** Controls for this section, rendered opposite the heading. */
    actions,
    children,
  }: {
    id: string;
    count?: SectionCount | null;
    tone?: HealthTone | null;
    caveat?: string | null;
    actions?: import("svelte").Snippet;
    children: import("svelte").Snippet;
  } = $props();

  const spec = $derived(healthSection(id));
  const domId = $derived(healthSectionDomId(id));
  const headingId = $derived(healthSectionHeadingId(id));
  const bounded = $derived(count ? isBounded(count) : false);
</script>

{#if spec}
  <!-- `tabindex="-1"` so a summary chip's jump can move keyboard focus here
       as well as the viewport. Without it `focus()` is a no-op on a section
       and the next Tab resumes from the top of the page, which is the usual
       way an in-page jump is unusable without a mouse. -->
  <section
    id={domId}
    tabindex="-1"
    aria-labelledby={headingId}
    aria-describedby={`${headingId}-desc`}
    data-health-section={id}
    class="scroll-mt-2 space-y-2 focus:outline-hidden {HEALTH_CONTENT_WIDTH}"
  >
    <span id={`${headingId}-desc`} class="sr-only">{spec.summary}</span>
    <div class="flex items-center justify-between gap-3 flex-wrap">
      <h3
        id={headingId}
        class="flex items-baseline gap-2 text-[11px] font-bold uppercase tracking-wider text-textSecondary"
      >
        <span>{spec.heading}</span>
        {#if count}
          <!-- The count and its disclosure are one string from one owner, so
               a bounded section cannot print only what survived the bound. -->
          <span
            class="font-mono normal-case tracking-normal {bounded
              ? 'text-amber-300'
              : 'text-textMuted'}">({formatSectionCount(count)})</span
          >
        {/if}
        {#if tone}
          <span
            class="px-1.5 py-0.5 rounded-full border text-[9px] font-semibold normal-case tracking-normal {toneChipClass(
              tone,
            )}">{toneLabel(tone)}</span
          >
        {/if}
      </h3>
      {@render actions?.()}
    </div>
    {#if caveat}
      <p class="text-[11px] text-amber-300 leading-relaxed">{caveat}</p>
    {/if}
    {@render children()}
  </section>
{/if}
