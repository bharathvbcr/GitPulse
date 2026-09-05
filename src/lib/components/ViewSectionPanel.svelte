<script lang="ts">
  import type { ViewTab } from "../repos/persist";
  import { panelId, tabId } from "../dom/tablist";

  /**
   * The panel half of a view's section tablist.
   *
   * `ViewSectionBar` declared `role="tab"` and `aria-controls` pointing at
   * nothing, because no element in the app carried `role="tabpanel"`. A tab
   * that controls a panel which does not exist is an announcement without a
   * referent: assistive technology reports the relationship and then cannot
   * follow it. This is the referent.
   */
  let {
    view,
    section,
    children,
    class: className = "",
  }: {
    view: ViewTab;
    /**
     * The active section id. Null while a view is resolving its default,
     * which is a real state and not an error — the panel still exists, it
     * just has no tab to point back at yet.
     */
    section: string | null;
    children: import("svelte").Snippet;
    class?: string;
  } = $props();
</script>

<div
  role="tabpanel"
  id={section ? panelId(view, section) : undefined}
  aria-labelledby={section ? tabId(view, section) : undefined}
  tabindex="0"
  class="flex-1 flex flex-col min-h-0 focus:outline-none {className}"
>
  {@render children()}
</div>
