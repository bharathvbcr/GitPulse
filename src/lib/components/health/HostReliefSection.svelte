<script lang="ts">
  import { explainError } from "../../workbench/client";
  import {
    endRequestOutcome,
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
  // The End request being followed: what it says is read from the published
  // status, so "requested" is never shown as "ended" and a refusal is shown.
  let endRequest = $state<{ tree: FrozenTree; at: number } | null>(null);
  const endNote = $derived.by(() => {
    if (!endRequest) return "";
    const current = $hostRelief;
    const status = current && (current.state === "fresh" || current.state === "stale") ? current.status : null;
    const result = status ? endRequestOutcome(status, endRequest.tree, endRequest.at) : { state: "waiting" as const };
    switch (result.state) {
      case "ended":
        return "Ended: every process of it is gone.";
      case "ending":
        return "Asked to end. Waiting for it to exit; anything still running after 10 seconds is killed.";
      case "refused":
        return `Not ended. ${result.detail}`;
      default:
        return "End requested. Waiting for the supervisor's next check.";
    }
  });

  /**
   * End or Pause one tree: the confirm comes first (it is `requestEndTree` /
   * `requestPauseTree`'s), and the request is only left when it is accepted.
   */
  async function askTree(
    tree: FrozenTree,
    ask: (tree: FrozenTree) => Promise<boolean>,
    sent: string | null,
  ) {
    if (requesting) return;
    requesting = true;
    error = "";
    note = "";
    endRequest = null;
    try {
      const at = Date.now();
      if (await ask(tree)) {
        if (sent === null) endRequest = { tree, at };
        else note = sent;
      }
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
  note={endNote || note}
  onresume={() => void resumeAll()}
  onend={(tree) => void askTree(tree, requestEndTree, null)}
  onpause={(tree) => void askTree(tree, requestPauseTree, "Pause requested. The supervisor pauses it on its next check, if it still qualifies.")}
/>
