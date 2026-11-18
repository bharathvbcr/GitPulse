<script lang="ts">
  /**
   * Explorer's side column: the file tree, or repository-wide content search,
   * both under ONE search field that FileTreePanel owns: it filters names by
   * default, and its contents toggle switches it to content search.
   *
   * Search used to be its own section of Code, so opening a hit swapped the
   * results out for the editor and coming back meant switching sections again.
   * Here the results stay beside the file they opened.
   *
   * The tree stays mounted while Contents is up (hidden, not unmounted): it
   * keeps its folds, filter and scroll position, and a search still running
   * keeps running while the tree is up.
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

  let contentPattern = $state("");
  let contentSearch: ContentSearch | undefined = $state();
</script>

<div class="h-full flex flex-col min-h-0 overflow-hidden" data-explorer-rail={rail}>
  <FileTreePanel
    {repoPath}
    {selectedFile}
    {revealRequest}
    {onSelectFile}
    {onPinFile}
    bind:contentQuery={contentPattern}
    onContentSubmit={() => void contentSearch?.run()}
  >
    {#snippet contents()}
      {#if searchMounted}
        <!-- A hit opens the way a click in the tree does (preview, and on a
             compact layout the editor comes forward) — including a hit in the
             file already selected, which a bare store selection would not
             announce. -->
        <ContentSearch
          bind:this={contentSearch}
          bind:pattern={contentPattern}
          {repoPath}
          onOpen={onSelectFile}
          {onSelectMatch}
        />
      {/if}
    {/snippet}
  </FileTreePanel>
</div>
