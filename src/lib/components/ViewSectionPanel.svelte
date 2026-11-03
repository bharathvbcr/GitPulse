<script lang="ts">
  import type { ViewTab } from "../repos/persist";
  import { panelId, tabId } from "../dom/tablist";
  import { repoStore } from "../stores/repoStore";

  /**
   * The panel half of a view's section tablist.
   *
   * `ViewSectionBar` declared `role="tab"` and `aria-controls` pointing at
   * nothing, because no element in the app carried `role="tabpanel"`. A tab
   * that controls a panel which does not exist is an announcement without a
   * referent: assistive technology reports the relationship and then cannot
   * follow it. This is the referent.
   *
   * It is also the one place the per-repository leaves are keyed. The shell
   * around them (Sidebar, the section bar, the status bar, this panel element)
   * survives a repository tab switch; what is inside a panel is a file tree,
   * a diff, a commit table, a measurement, each with scroll offsets, open
   * tabs, drafts and in-flight requests that belong to one repository. Keying
   * the content by `currentPath` is what stops those leaking into the next
   * repository. Untangling a leaf so it can survive a switch too is a
   * per-leaf decision; until it is made, the leaf is rebuilt.
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
  class="flex-1 flex flex-col min-h-0 focus:outline-hidden {className}"
>
  {#key $repoStore.currentPath}
    {@render children()}
  {/key}
</div>
