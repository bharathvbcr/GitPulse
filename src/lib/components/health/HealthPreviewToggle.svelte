<script lang="ts">
  import { HEALTH_PREVIEW_ROWS } from "../../health/sections";
  import { expandLabel, overflowsPreview } from "../../ui/previewList";

  /**
   * The "Show all N" control under a Health list.
   *
   * Health's lists run long — hundreds of outdated packages, thousands of
   * secret findings — and each one pushed every section below it off the
   * page. A list opens on its first {@link HEALTH_PREVIEW_ROWS} rows and this
   * control reveals the rest. The slicing is `previewSlice`'s, shared with the
   * GitHub rail, so "show all" keeps one meaning across the app.
   *
   * `total` is the number of rows the list holds, never what the scan
   * observed: the section heading's count states the scan's own number and
   * any cap, and this control speaks only for rows it can actually reveal.
   * It renders nothing for a list too short to be collapsed.
   */
  let {
    total,
    /** Plural noun for the label: "Show all 40 findings". */
    noun,
    expanded = $bindable(false),
  }: {
    total: number;
    noun: string;
    expanded?: boolean;
  } = $props();
</script>

{#if overflowsPreview(total, HEALTH_PREVIEW_ROWS)}
  <button
    type="button"
    class="gp-btn w-full"
    data-size="sm"
    aria-expanded={expanded}
    data-health-preview-toggle
    onclick={() => (expanded = !expanded)}
  >
    {expandLabel(total, expanded, noun)}
  </button>
{/if}
