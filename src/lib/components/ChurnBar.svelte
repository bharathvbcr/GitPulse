<script lang="ts">
  let {
    additions = 0,
    deletions = 0,
  }: {
    additions?: number;
    deletions?: number;
  } = $props();

  let total = $derived(additions + deletions);
  let addPct = $derived(total === 0 ? 0 : (additions / total) * 100);
  let delPct = $derived(total === 0 ? 0 : (deletions / total) * 100);
</script>

{#if total > 0}
  <span
    class="inline-flex items-center gap-1 font-mono text-micro shrink-0"
    title="+{additions} / -{deletions} lines"
    role="img"
    aria-label="{additions} additions, {deletions} deletions"
  >
    <span class="text-ok">+{additions}</span>
    <span class="text-danger">-{deletions}</span>
    <span class="w-8 h-1 rounded-full overflow-hidden bg-edgeSoft flex" aria-hidden="true">
      <span class="h-full bg-ok" style="width: {addPct}%"></span>
      <span class="h-full bg-danger" style="width: {delPct}%"></span>
    </span>
  </span>
{/if}
