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
    class="inline-flex items-center gap-1 font-mono text-[10px] shrink-0"
    title="+{additions} / -{deletions} lines"
  >
    <span class="text-emerald-400">+{additions}</span>
    <span class="text-rose-400">-{deletions}</span>
    <span class="w-8 h-1 rounded-full overflow-hidden bg-border/70 flex">
      <span class="h-full bg-emerald-500" style="width: {addPct}%"></span>
      <span class="h-full bg-rose-500" style="width: {delPct}%"></span>
    </span>
  </span>
{/if}
