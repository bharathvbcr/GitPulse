<script lang="ts">
  // text-[10px] in a comment is not a class
  const note = "text-xs";
</script>

<!-- <div class="text-[11px]"> a commented-out element -->
<div class="text-[var(--row-text)] text-[length:var(--x)] text-[1.2rem] text-xs text-micro text-lg text-display">
  arbitrary values that are not pixel sizes
</div>
