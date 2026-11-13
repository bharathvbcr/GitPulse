<script lang="ts">
  import ContentSearch from "../src/lib/components/ContentSearch.svelte";

  // Explorer's one search field lives in FileTreePanel; this stands in for it
  // in Contents mode so the results panel is checked on its own.
  let pattern = $state("");
  let search: ContentSearch | undefined = $state();
</script>

<!-- At the width it is drawn at: Explorer's side column (w-72, 288px).
     Search used to be a full Code section; the layout checks only mean
     something at the width users now see it. -->
<main class="flex flex-col h-screen w-72 bg-background text-textPrimary">
  <div class="px-2 pt-2">
    <input
      class="gp-field w-full font-mono"
      type="search"
      placeholder="Search file contents"
      aria-label="Search file contents"
      bind:value={pattern}
      onkeydown={(event) => { if (event.key === "Enter") void search?.run(); }}
    />
  </div>
  <ContentSearch bind:this={search} bind:pattern />
</main>
