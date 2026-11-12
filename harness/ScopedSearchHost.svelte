<script lang="ts">
  /**
   * A viewer mounted as a page's own pane, with the view bar's search field
   * above it. In the app, History provides "history:diff" around its Diff
   * section and Code provides "code:explorer" around its file; the field
   * lives in row 2. This stands the two in one column so a harness can drive
   * the field against a real viewer.
   */
  import type { Component } from "svelte";
  import PageSearch from "../src/lib/components/PageSearch.svelte";
  import { providePageSearchScope, type PageScope } from "../src/lib/search/pageSearch.svelte";

  let { scope, viewer, viewerProps = {} }: { scope: PageScope; viewer: Component<Record<string, unknown>>; viewerProps?: Record<string, unknown> } = $props();

  // A page provides its place once, when it mounts, as History and Code do.
  // svelte-ignore state_referenced_locally
  providePageSearchScope(scope);
  const Viewer = $derived(viewer);
</script>

<div class="flex h-full min-w-0 flex-1 flex-col">
  <div class="flex h-9 shrink-0 items-center px-2" data-scoped-bar><PageSearch /></div>
  <div class="flex min-h-0 flex-1 flex-col"><Viewer {...viewerProps} /></div>
</div>
