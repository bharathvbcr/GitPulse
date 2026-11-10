<script lang="ts">
  import { LAYERS } from "../ui/layers";
</script>

<div class="relative z-0 z-auto">stacking context only</div>
<div style="z-index: var(--z-modal)">token</div>
<div style:z-index={LAYERS.MODAL}>layers</div>

<style>
  .a {
    z-index: 0;
  }
</style>
