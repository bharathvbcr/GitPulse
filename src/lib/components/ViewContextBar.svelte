<script lang="ts">
  /**
   * A view's own toolbar row, drawn only when the view has something to put
   * in it.
   *
   * This row used to be the section switcher: a full-width strip under the
   * header on every view, holding the lens tabs plus whatever the current
   * section added at the right. The tabs moved into the header switcher
   * (`ViewSectionTabs`, beside the view that owns them), so what is left is
   * the per-view content: History's list segment, Blame's file path, Work's
   * conflict count. A view with none of that renders no row at all, and its
   * pane gets the height back — callers wrap this in the condition that says
   * whether there is anything to show.
   *
   * At 1600px and wider the header's view bar has room for this content, so
   * the group moves into the bar's slot (`ui/headerSlots`) and the pane's row
   * collapses. It is the same DOM node either way: the controls inside keep
   * their state, focus and handlers across the move.
   */
  import { foldTarget } from "../ui/headerSlots";

  let {
    children,
    label,
  }: {
    children: import("svelte").Snippet;
    /** What the row is, for assistive tech: "History filter", "Blame file". */
    label: string;
  } = $props();

  let home: HTMLDivElement | undefined = $state();
  let content: HTMLDivElement | undefined = $state();
  const folded = $derived($foldTarget !== null);

  $effect(() => {
    const target = $foldTarget;
    const node = content;
    const row = home;
    if (!target || !node || !row) return;
    target.appendChild(node);
    return () => {
      // Back to the pane's row first: when the whole bar is being torn down
      // the row goes with it, and the node must not outlive its owner in the
      // header's slot.
      row.appendChild(node);
    };
  });
</script>

<!-- No rule and no band of its own: its controls (a pill search, a chip)
     float on the pane, so the row reads as part of the view below it rather
     than a second strip under the header. -->
<div
  bind:this={home}
  data-context-row
  class="shrink-0 px-3 pt-1 select-none min-w-0 {folded ? 'hidden' : 'h-10 flex items-center'}"
>
  <div
    bind:this={content}
    role="group"
    aria-label={label}
    class="flex min-w-0 flex-1 items-center gap-3"
  >
    {@render children()}
  </div>
</div>
