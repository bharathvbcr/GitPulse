<script lang="ts">
  /**
   * A view's own toolbar row, drawn only when the view has something to put
   * in it.
   *
   * This row used to be the section switcher: a full-width strip under the
   * header on every view, holding the lens tabs plus whatever the current
   * section added at the right. The tabs moved into the header switcher
   * (`ViewSectionTabs`, beside the view that owns them), so what is left is
   * the per-view content: History's commit filter, Blame's file path, Work's
   * conflict count. A view with none of that renders no row at all, and its
   * pane gets the height back — callers wrap this in the condition that says
   * whether there is anything to show.
   */
  let {
    children,
    label,
  }: {
    children: import("svelte").Snippet;
    /** What the row is, for assistive tech: "History filter", "Blame file". */
    label: string;
  } = $props();
</script>

<div
  role="group"
  aria-label={label}
  class="h-9 shrink-0 px-3 flex items-center gap-3 border-b border-border/60 gp-section-edge bg-surface/40 select-none min-w-0"
>
  {@render children()}
</div>
