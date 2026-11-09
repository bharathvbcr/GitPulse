<script lang="ts">
  import { motionDuration } from "../motion/easing";
  const wait = motionDuration(260);
</script>

<div class="duration-500 ease-in-out">tailwind literals</div>
<div in:fade={{ duration: 180 }}>transition params</div>

<style>
  .a {
    transition: opacity 120ms;
  }
  .b {
    animation-timing-function: cubic-bezier(0.4, 0, 0.2, 1);
  }
</style>
