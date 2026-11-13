<script lang="ts">
  import PageHeader from "./PageHeader.svelte";
</script>

<PageHeader title="Storage" />
<div class="gp-page-body">
  <div class="gp-notice max-w-2xl" data-tone="danger">A notice capped in a narrow column.</div>
  <section class="gp-card max-w-4xl p-4">An outgoing-commits card capped at 4xl.</section>
  <div class="@md:max-w-xl">A variant does not hide the cap.</div>
  <select class="gp-select max-w-40" data-width-cap="">An empty reason is not a reason.</select>
  <span class="max-w-[32rem] truncate">An arbitrary length is a cap.</span>
</div>
