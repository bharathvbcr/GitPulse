<script lang="ts">
  import { expandLabel, LIST_PREVIEW_ROWS, overflowsPreview } from "../ui/previewList";

  /**
   * The "Show all N" control under a long list, anywhere in the app.
   *
   * A list opens on its first {@link LIST_PREVIEW_ROWS} rows (sliced by
   * `previewSlice`) and this control reveals the rest, then collapses them
   * again. One control is how "show all" keeps one meaning and one look on
   * every report page.
   *
   * `total` is the number of rows the list holds, never what a scan observed:
   * a section's own count states the scan's number and any cap, and this
   * control speaks only for rows it can actually reveal. It renders nothing
   * for a list too short to be collapsed.
   */
  let {
    total,
    /** Plural noun for the label: "Show all 40 findings". */
    noun,
    expanded = $bindable(false),
    /** Rows shown collapsed; the app-wide default unless a list has a reason. */
    previewRows = LIST_PREVIEW_ROWS,
    /** Spacing from the owner's layout (`mt-2` under a rail list); never colour or size. */
    class: className = "",
  }: {
    total: number;
    noun: string;
    expanded?: boolean;
    previewRows?: number;
    class?: string;
  } = $props();
</script>

{#if overflowsPreview(total, previewRows)}
  <button
    type="button"
    class="gp-btn w-full {className}"
    data-size="sm"
    aria-expanded={expanded}
    data-list-preview-toggle
    onclick={() => (expanded = !expanded)}
  >
    {expandLabel(total, expanded, noun)}
  </button>
{/if}
