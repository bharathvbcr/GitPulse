<script lang="ts">
  import { MOTION } from "../motion/tokens";
  // duration: 180 in a comment
</script>

<p>Duration: 5 minutes</p>
<div in:fade={{ duration: MOTION.base }} out:fade={{ duration: 0 }}>token and zero</div>

<style>
  .a {
    transition: opacity var(--gp-dur-fast) var(--gp-ease-out);
    transition-duration: var(--gp-dur-base);
  }
</style>
