<script lang="ts">
  /**
   * Names a branch for the work in progress with the local model, beside the
   * field that takes a branch name.
   *
   * Offered only while the model is ready: the suggestion is the only thing
   * this control does, so a disabled button would be a permanent dead end on a
   * machine with no model server. `onUse` lets a host field adopt the name; the
   * Create button makes the branch in the repository the suggestion was drawn
   * from, which is captured when the suggestion is asked for.
   */
  import { GitBranch } from "@lucide/svelte";
  import { harnessStore } from "../stores/harnessStore";
  import { repoStore, type MutationTarget } from "../stores/repoStore";
  import { describeFailure } from "../ui/copy/describeFailure";

  let { onUse }: { onUse?: (name: string) => void } = $props();

  let branchSuggestion = $state("");
  /** The repository the suggestion was made for; the branch is created there. */
  let branchTarget = $state<MutationTarget | null>(null);
  let branchWarnings = $state<string[]>([]);
  let isSuggesting = $state(false);
  let branchError = $state<string | null>(null);

  let ready = $derived(Boolean($harnessStore.ai?.ready));

  /** "Couldn't <verb> <object>. <Reason>." for an error line. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }

  async function suggestBranch() {
    const path = $repoStore.currentPath;
    if (!path) return;
    isSuggesting = true;
    branchError = null;
    branchSuggestion = "";
    branchTarget = repoStore.captureTarget();
    branchWarnings = [];
    try {
      const result = await harnessStore.suggestBranchName(path);
      branchSuggestion = result.text;
      branchWarnings = result.warnings;
    } catch (err: unknown) {
      branchError = failure("suggest", "a branch name", err);
    } finally {
      isSuggesting = false;
    }
  }

  async function createSuggestedBranch() {
    if (!branchSuggestion) return;
    // createBranch resolves { ok: false } on a refusal or a collision; it never
    // throws, so the suggestion is kept and the reason shown unless it was made.
    const outcome = await repoStore.createBranch(branchSuggestion, undefined, branchTarget);
    if (outcome.ok) {
      branchSuggestion = "";
    } else {
      branchError = outcome.error ?? "Couldn't create the branch.";
    }
  }
</script>

{#if ready && $repoStore.currentPath}
  <div class="space-y-1.5" data-testid="branch-name-suggestion">
    <div class="flex items-center gap-1.5">
      <button
        type="button"
        onclick={suggestBranch}
        disabled={isSuggesting}
        class="gp-btn"
        data-size="sm"
        title="Name a branch for the work in progress"
      >
        <GitBranch size={11} />
        <span>{isSuggesting ? "Thinking…" : "Suggest a name"}</span>
      </button>
      {#if branchSuggestion}
        <input
          bind:value={branchSuggestion}
          aria-label="Suggested branch name"
          class="gp-field flex-1 min-w-0 font-mono"
          onkeydown={(event) => {
            // Inside the worktree form, Enter would submit it; the buttons are the explicit choices.
            if (event.key === "Enter") event.preventDefault();
          }}
        />
        {#if onUse}
          <button
            type="button"
            onclick={() => onUse(branchSuggestion)}
            class="gp-btn"
            data-size="sm"
          >
            Use
          </button>
        {/if}
        <button type="button" onclick={createSuggestedBranch} class="gp-btn" data-size="sm">
          Create
        </button>
      {/if}
    </div>
    {#if branchError}
      <div class="gp-notice" data-tone="danger" role="alert">{branchError}</div>
    {/if}
    {#each branchWarnings as warning}
      <div class="gp-notice" data-tone="warn" role="status">{warning}</div>
    {/each}
  </div>
{/if}
