<script lang="ts">
  /**
   * The content area of a page: the scroll area, the pane-width container and the
   * padded column of sections, in one place (`gp-page` + `gp-page-body`,
   * design language section 7a).
   *
   * Nothing in it is capped. Padding and the gap between sections step up as the
   * pane widens; a list of cards uses `gp-page-grid`; the `@md:`/`@xl:` variants
   * inside a page follow this container's width, never the window's.
   */
  import type { Snippet } from "svelte";

  interface Props {
    children: Snippet;
    /** Extra utilities on the scroll area. */
    class?: string;
    /** Extra utilities on the padded column (`pb-4`, a tighter `gap-*`). */
    bodyClass?: string;
    /** Hook for a page's test or harness. */
    testId?: string;
  }

  let { children, class: className = "", bodyClass = "", testId }: Props = $props();
</script>

<div class={["gp-page", className]} data-page-body data-testid={testId}>
  <div class={["gp-page-body", bodyClass]}>{@render children()}</div>
</div>
