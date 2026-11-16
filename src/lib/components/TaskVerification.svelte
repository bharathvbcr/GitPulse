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
  import { describeFailure } from "../ui/copy/describeFailure";
  import {
    postAvailability, postConfirm, postedSummary, postVerifyResult, setVerifyStatus, verifyStatusEnabled,
  } from "../workbench/verifyStatus";
  import Toggle from "./Toggle.svelte";
  import VerificationChip from "./VerificationChip.svelte";

  let { taskId, revision, repositoryPath = "" }: { taskId: string; revision: number; repositoryPath?: string } = $props();
  // The repository's opt-in, as the app read it. `null` is "not read yet": the
  // action stays disabled until a read says `true`.
  let optedIn = $state<boolean | null>(null);
  let optInError = $state("");
  let savingOptIn = $state(false);
  let withComment = $state(false);
  let posting = $state(false);
  let posted = $state("");
  const availability = $derived(postAvailability(optedIn, optInError, repositoryPath));
  let verification = $state<Verification | null>(null);
  let busy = $state(false);
  let error = $state("");
  let activeTask = "";
  let activeRepo = "";

  /** "Couldn't <verb> <object>. <Reason>." for the error line. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }
  async function refresh(targetTask = taskId) {
    try {
      const next = await getVerification(targetTask);
      if (targetTask === taskId) { verification = next; error = ""; }
    } catch (e) {
      if (targetTask === taskId) error = failure("read", "the verification", e);
    }
  }
  async function verify() {
    if (busy) return;
    busy = true; error = "";
    try { await runVerification(taskId); }
    catch (e) { error = failure("start", "the verification", e); }
    finally { busy = false; }
    await refresh();
  }
  async function readOptIn(path: string) {
    optedIn = null; optInError = "";
    if (path.trim() === "") return;
    try { const enabled = await verifyStatusEnabled(path); if (path === repositoryPath) optedIn = enabled === true; }
    catch (e) { if (path === repositoryPath) optInError = explainError(e); }
  }
  async function toggleOptIn(next: boolean) {
    if (savingOptIn || posting || repositoryPath.trim() === "") return;
    const target = repositoryPath;
    savingOptIn = true; posted = ""; error = "";
    try {
      await setVerifyStatus(target, next);
      if (!next && target === repositoryPath) withComment = false;
      if (target === repositoryPath) await readOptIn(target);
    } catch (e) {
      if (target === repositoryPath) error = failure("change", "the GitHub status setting", e);
    } finally {
      savingOptIn = false;
    }
  }
  async function post() {
    if (posting || savingOptIn || !availability.allowed) return;
    const targetRepo = repositoryPath;
    const targetTask = taskId;
    const comment = withComment;
    posted = ""; error = "";
    if (!(await askConfirm({ ...postConfirm(comment) }))) return;
    if (targetRepo !== repositoryPath || targetTask !== taskId || !availability.allowed) return;
    posting = true;
    try {
      const result = await postVerifyResult(targetRepo, targetTask, { comment });
      if (targetRepo === repositoryPath && targetTask === taskId) posted = postedSummary(result);
    } catch (e) {
      if (targetRepo === repositoryPath && targetTask === taskId) error = failure("post", "the verify result", e);
    } finally { posting = false; }
  }
  $effect(() => {
    const id = taskId;
    void revision;
    if (id !== activeTask) {
      activeTask = id;
      verification = null;
      posted = "";
      error = "";
    }
    void refresh(id);
  });
  $effect(() => {
    const path = repositoryPath;
    if (path !== activeRepo) {
      activeRepo = path;
      withComment = false;
      posted = "";
      error = "";
    }
    void readOptIn(path);
  });
  onMount(() =>
    // Stops while the window is hidden and polls once when it returns.
    createAdaptiveTimer(() => { if (verification?.state === "running") void refresh(); }, 4000),
  );
</script>

<section class="verification" data-testid="task-verification">
  {#if verification}<VerificationChip {verification} />{/if}
  <button type="button" class="gp-btn" data-testid="task-verify" disabled={busy || verification?.state === "running"} onclick={() => void verify()}>
    {busy ? "Starting…" : "Verify"}
  </button>
  <button type="button" class="gp-btn" data-testid="task-verify-post" disabled={!availability.allowed || posting || savingOptIn} title={availability.allowed ? "Post the latest verdict to GitHub as a commit status" : availability.reason} onclick={() => void post()}>
    {posting ? "Posting…" : "Post verify result"}
  </button>
  {#if repositoryPath.trim() !== ""}
    <div class="basis-full">
      <Toggle
        id="task-verify-opt-in"
        checked={optedIn === true}
        disabled={savingOptIn || posting || optedIn === null || Boolean(optInError)}
        label="Allow posting verification results to GitHub for this repository"
        onchange={(next) => void toggleOptIn(next)}
      />
      {#if availability.allowed}
        <Toggle
          id="task-verify-comment"
          checked={withComment}
          disabled={posting || savingOptIn}
          label="Also comment on open pull requests"
          onchange={(next) => { withComment = next; }}
        />
      {/if}
    </div>
  {/if}
  <!-- The reason is the button's tooltip and, for a screen reader, this note; it is
       drawn only when the setting could not be read, so a repository that simply
       has not opted in does not push the sheet's sections down. -->
  {#if !availability.allowed}
    {#if optInError}<div class="gp-notice" data-tone="unknown" role="status" data-testid="task-verify-post-note">{availability.reason}</div>
    {:else}<p class="note" data-testid="task-verify-post-note">{availability.reason}</p>{/if}
  {/if}
  {#if posted}<div class="gp-notice" data-tone="ok" role="status" data-testid="task-verify-posted">{posted}</div>{/if}
  {#if error}<div class="gp-notice" data-tone="danger" role="alert" data-testid="task-verify-error">{error}</div>{/if}
</section>

<style>
  .verification{display:flex;flex-wrap:wrap;align-items:center;gap:.5rem;margin-bottom:.75rem}
  .note{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0);white-space:nowrap}
  .verification .gp-notice{flex-basis:100%;margin:0}
</style>
