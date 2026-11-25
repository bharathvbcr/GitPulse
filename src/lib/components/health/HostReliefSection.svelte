<script lang="ts">
  import { explainError } from "../../workbench/client";
  import { hostRelief, reliefStatusKey, requestResumeAll } from "../../health/hostRelief";
  import HostReliefPanel from "./HostReliefPanel.svelte";

  /**
   * The Health row for the memory-pressure supervisor: reads the published
   * status (kept fresh while this is on screen) and sends the "Resume all"
   * request. What it shows is `HostReliefPanel`.
   */
  let requesting = $state(false);
  // The status the request was made against. The note that it was sent lasts
  // only while the supervisor's account is still that one: once it moves on
  // (the pause ended, or it reported again) the note is gone.
  let requestedAgainst = $state<string | null>(null);
  const requested = $derived(requestedAgainst !== null && requestedAgainst === reliefStatusKey($hostRelief));
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
      const against = reliefStatusKey($hostRelief);
      await requestResumeAll();
      // The list keeps showing what the supervisor publishes; this only says
      // the request was left for it.
      requestedAgainst = against;
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
