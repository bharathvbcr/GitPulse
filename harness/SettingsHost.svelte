<script lang="ts">
  import SettingsModal from "../src/lib/components/SettingsModal.svelte";

  let isOpen = $state(true);
  // Exposed so the probe can drive the closed -> open transition, which is
  // the only thing that can prove the on-open re-read actually re-runs.
  (globalThis as Record<string, unknown>).__gpSetOpen = (next: boolean) => (isOpen = next);
</script>

<SettingsModal {isOpen} onClose={() => (isOpen = false)} />
