<script lang="ts">
  import { LAYERS } from "../ui/layers";
</script>

<!--
  The veil shown while a file or folder is dragged over the window. App owns
  when it shows (nativeShell's reduceFileDrop decides from the native drag
  events; App adds a short hide grace); this owns what it looks like. It takes
  no pointer events, so the drop reaches the window underneath.
-->
<div
  class="gp-overlay absolute inset-0 bg-accent/10 p-4 flex items-center justify-center pointer-events-none gp-gpu"
  style="z-index: {LAYERS.DROP_OVERLAY}"
  data-testid="drop-overlay"
>
  <div class="w-full h-full rounded-2xl border-2 border-dashed border-accent/65 bg-accent/5 flex items-center justify-center">
    <div class="gp-pop px-6 py-4 rounded-2xl bg-surface border border-accent/35 text-sm font-semibold text-textPrimary shadow-float">
      Drop a Git repository to open
    </div>
  </div>
</div>
