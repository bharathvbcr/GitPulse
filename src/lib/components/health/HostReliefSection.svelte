<script lang="ts">
  import { explainError } from "../../workbench/client";
  import { hostRelief, requestResumeAll } from "../../health/hostRelief";
  import HostReliefPanel from "./HostReliefPanel.svelte";

  /**
   * The Health row for the memory-pressure supervisor: reads the published
   * status (kept fresh while this is on screen) and sends the "Resume all"
   * request. What it shows is `HostReliefPanel`.
   */
  let requesting = $state(false);
  let requested = $state(false);
  let error = $state("");
  // Ages are read from the clock each time the status is read, which is as
  // often as the poll that keeps it fresh.
  const now = $derived.by(() => {
    void $hostRelief;
    return Date.now();
  });

  async function resumeAll() {
    if (requesting) return;
    requesting = true;
    error = "";
    try {
      await requestResumeAll();
      // The list keeps showing what the supervisor publishes; this only says
      // the request was left for it.
      requested = true;
    } catch (cause) {
      error = explainError(cause);
    } finally {
      requesting = false;
    }
  }
</script>

<HostReliefPanel
  outcome={$hostRelief}
  {now}
  {requesting}
  {requested}
  {error}
  onresume={() => void resumeAll()}
/>
