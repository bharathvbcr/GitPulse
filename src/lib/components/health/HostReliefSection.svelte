<script lang="ts">
  import { explainError } from "../../workbench/client";
  import {
    hostRelief,
    reliefStatusKey,
    requestEndTree,
    requestPauseTree,
    requestResumeAll,
    type FrozenTree,
  } from "../../health/hostRelief";
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

  let note = $state("");

  /**
   * End or Pause one tree: the confirm comes first (it is `requestEndTree` /
   * `requestPauseTree`'s), and the request is only left when it is accepted.
   */
  async function askTree(
    tree: FrozenTree,
    ask: (tree: FrozenTree) => Promise<boolean>,
    sent: string,
  ) {
    if (requesting) return;
    requesting = true;
    error = "";
    note = "";
    try {
      if (await ask(tree)) note = sent;
    } catch (cause) {
      error = explainError(cause);
    } finally {
      requesting = false;
    }
  }

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
  {note}
  onresume={() => void resumeAll()}
  onend={(tree) => void askTree(tree, requestEndTree, "End requested. The supervisor ends it on its next check.")}
  onpause={(tree) => void askTree(tree, requestPauseTree, "Pause requested. The supervisor pauses it on its next check, if it still qualifies.")}
/>
