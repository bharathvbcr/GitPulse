<script lang="ts">
  /**
   * A saved task's verification: the verdict chip and a Verify action.
   * Reads through `verifications.status` (the store, so it works with the
   * sidecar down) and polls while a verification is running.
   */
  import { onMount } from "svelte";
  import { explainError } from "../workbench/client";
  import { getVerification, runVerification, type Verification } from "../workbench/verification";
  import VerificationChip from "./VerificationChip.svelte";

  let { taskId, revision }: { taskId: string; revision: number } = $props();
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
  $effect(() => { void revision; void refresh(); });
  onMount(() => {
    const timer = setInterval(() => { if (verification?.state === "running") void refresh(); }, 4000);
    return () => clearInterval(timer);
  });
</script>

<section class="verification" data-testid="task-verification">
  {#if verification}<VerificationChip {verification} />{/if}
  <button type="button" class="gp-btn" data-testid="task-verify" disabled={busy || verification?.state === "running"} onclick={() => void verify()}>
    {busy ? "Starting..." : "Verify"}
  </button>
  {#if error}<p class="error" role="alert" data-testid="task-verify-error">{error}</p>{/if}
</section>

<style>
  .verification{display:flex;flex-wrap:wrap;align-items:center;gap:.5rem;margin-bottom:.75rem}
  .error{flex-basis:100%;margin:0;color:var(--c-status-red);font-size:.8rem}
</style>
