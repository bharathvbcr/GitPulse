<script lang="ts">
  /**
   * Mounts the notification-review bridge as its own chunk.
   *
   * The bridge is never on screen at first paint: it reads the activation a
   * clicked banner left behind and opens the task it names. That read is
   * persisted by the backend, so a bridge that arrives a few milliseconds after
   * the entry chunk misses nothing, and its ~3 kB stays out of the entry chunk
   * (see lazyChunks.test.ts). The component itself is
   * `NativeNotificationBridgeView.svelte`.
   */
  import LazyMount from "./LazyMount.svelte";
  import { loadNativeNotificationBridge } from "./lazyLoaders";
</script>

<LazyMount load={loadNativeNotificationBridge} name="Notification review" />
