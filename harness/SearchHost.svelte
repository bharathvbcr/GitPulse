<script lang="ts">
  import ContentSearch from "../src/lib/components/ContentSearch.svelte";
</script>

<!-- At the width it is drawn at: Explorer's side column (w-72, 288px).
     Search used to be a full Code section; the layout checks only mean
     something at the width users now see it. -->
<main class="flex flex-col h-screen w-72 bg-background text-textPrimary">
  <ContentSearch />
</main>
