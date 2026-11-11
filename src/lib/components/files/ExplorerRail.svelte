<script lang="ts">
  /**
   * Explorer's side column: the file tree, or repository-wide content search.
   *
   * Search used to be its own section of Code, so opening a hit swapped the
   * results out for the editor and coming back meant switching sections again.
   * Here the results stay beside the file they opened.
   *
   * Both panes stay mounted once shown and the inactive one is hidden, not
   * unmounted: the tree keeps its folds, filter and scroll position, and a
   * search still running keeps running while the tree is up.
   */
  import ContentSearch from "../ContentSearch.svelte";
  import FileTreePanel from "./FileTreePanel.svelte";
  import { explorerLens } from "../../files/explorerLens";

  let {
    repoPath = null,
    selectedFile = null,
    revealRequest = null,
    onSelectFile,
    onPinFile,
    onSelectMatch,
  }: {
    repoPath?: string | null;
    selectedFile?: string | null;
    revealRequest?: { path: string; nonce: number } | null;
    onSelectFile?: (path: string) => void;
    onPinFile?: (path: string) => void;
    onSelectMatch?: (path: string, line: number | null, column: number | null) => void;
  } = $props();

  const rail = $derived($explorerLens.rail);
  // Search costs nothing until someone asks for it, and then stays.
  let searchMounted = $state(false);
  $effect(() => {
    if (rail === "search") searchMounted = true;
  });
</script>

<div class="h-full flex flex-col min-h-0 overflow-hidden" data-explorer-rail={rail}>
  <div class="flex-1 min-h-0 flex flex-col" class:hidden={rail !== "files"}>
    <FileTreePanel {repoPath} {selectedFile} {revealRequest} {onSelectFile} {onPinFile} />
  </div>
  {#if searchMounted}
    <div
      class="flex-1 min-h-0 flex flex-col border-r border-border/70 bg-surface/50"
      class:hidden={rail !== "search"}
    >
      <!-- A hit opens the way a click in the tree does (preview, and on a
           compact layout the editor comes forward) — including a hit in the
           file already selected, which a bare store selection would not
           announce. -->
      <ContentSearch {repoPath} onOpen={onSelectFile} {onSelectMatch} />
    </div>
  {/if}
</div>
