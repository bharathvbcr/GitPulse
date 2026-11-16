<script lang="ts">
  import PageHeader from "./PageHeader.svelte";
</script>

<PageHeader title="Storage" />
<div class="gp-page-body">
  <div class="gp-notice" data-tone="danger">A notice fills the page body.</div>
  <section class="gp-card p-4">A card fills the page body.</section>
  <div class="gp-page-grid">
    <div class="gp-card">One card of several.</div>
  </div>
  <p class="gp-prose text-textMuted">Running text takes the one measure, which is CSS and not a token.</p>
  <select class="gp-select max-w-40" data-width-cap="a single select, sized to its option labels">One control, with its reason.</select>
  <span class="min-w-0 max-w-full truncate">Full is the parent's own width.</span>
  <span class="max-w-[40%] truncate">A share of the parent is not a cap.</span>
  <div class="max-w-none">None removes one.</div>
</div>
