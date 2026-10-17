<script lang="ts">
  /**
   * A saved task's verification: the verdict chip and a Verify action.
   * Reads through `verifications.status` (the store, so it works with the
   * sidecar down) and polls while a verification is running.
   */
  import { onMount } from "svelte";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  import { explainError } from "../workbench/client";
  import { getVerification, runVerification, type Verification } from "../workbench/verification";
  import { askConfirm } from "../stores/modalStore";
  import {
    POST_CONFIRM, postAvailability, postedSummary, postVerifyResult, verifyStatusEnabled,
  } from "../workbench/verifyStatus";
  import VerificationChip from "./VerificationChip.svelte";

  let { taskId, revision, repositoryPath = "" }: { taskId: string; revision: number; repositoryPath?: string } = $props();
  // The repository's opt-in, as the app read it. `null` is "not read yet": the
  // action stays disabled until a read says `true`.
  let optedIn = $state<boolean | null>(null);
  let optInError = $state("");
  let posting = $state(false);
  let posted = $state("");
  const availability = $derived(postAvailability(optedIn, optInError, repositoryPath));
  let verification = $state<Verification | null>(null);
  let busy = $state(false);
  let error = $state("");

  async function refresh() {
    try { verification = await getVerification(taskId); error = ""; }
    catch (e) { error = explainError(e); }
  }
  async function verify() {
    if (busy) return;
    busy = true; error = "";
    try { await runVerification(taskId); }
    catch (e) { error = explainError(e); }
    finally { busy = false; }
    await refresh();
  }
  async function readOptIn(path: string) {
    optedIn = null; optInError = "";
    if (path.trim() === "") return;
    try { const enabled = await verifyStatusEnabled(path); if (path === repositoryPath) optedIn = enabled === true; }
    catch (e) { if (path === repositoryPath) optInError = explainError(e); }
  }
  async function post() {
    if (posting || !availability.allowed) return;
    posted = ""; error = "";
    if (!(await askConfirm({ ...POST_CONFIRM }))) return;
    posting = true;
    try { posted = postedSummary(await postVerifyResult(repositoryPath, taskId)); }
    catch (e) { error = explainError(e); }
    finally { posting = false; }
  }
  $effect(() => { void revision; void refresh(); });
  $effect(() => { void readOptIn(repositoryPath); });
  onMount(() =>
    // Stops while the window is hidden and polls once when it returns.
    createAdaptiveTimer(() => { if (verification?.state === "running") void refresh(); }, 4000),
  );
</script>

<section class="verification" data-testid="task-verification">
  {#if verification}<VerificationChip {verification} />{/if}
  <button type="button" class="gp-btn" data-testid="task-verify" disabled={busy || verification?.state === "running"} onclick={() => void verify()}>
    {busy ? "Starting..." : "Verify"}
  </button>
  <button type="button" class="gp-btn" data-testid="task-verify-post" disabled={!availability.allowed || posting} title={availability.allowed ? "Post the latest verdict to GitHub as a commit status" : availability.reason} onclick={() => void post()}>
    {posting ? "Posting..." : "Post verify result"}
  </button>
  <!-- The reason is the button's tooltip and, for a screen reader, this note; it is
       drawn only when the setting could not be read, so a repository that simply
       has not opted in does not push the sheet's sections down. -->
  {#if !availability.allowed}<p class={optInError ? "error" : "note"} data-testid="task-verify-post-note">{availability.reason}</p>{/if}
  {#if posted}<p class="note" role="status" data-testid="task-verify-posted">{posted}</p>{/if}
  {#if error}<p class="error" role="alert" data-testid="task-verify-error">{error}</p>{/if}
</section>

<style>
  .verification{display:flex;flex-wrap:wrap;align-items:center;gap:.5rem;margin-bottom:.75rem}
  .note{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0);white-space:nowrap}
  .error{flex-basis:100%;margin:0;color:var(--c-status-red);font-size:.8rem}
</style>
