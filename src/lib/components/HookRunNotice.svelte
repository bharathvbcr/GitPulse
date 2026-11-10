<script lang="ts">
  /**
   * Shown while a commit, merge or other hook-running git command is in
   * flight. After a few seconds it says a repository hook may be what is
   * taking the time and offers to stop it — the command is no longer killed
   * at 90 s, so the user is the one who decides when a slow hook has run long
   * enough (the backend still stops it at its own, much later, deadline).
   */
  import { Square } from "@lucide/svelte";
  import { repoStore } from "../stores/repoStore";
  import { describeFailure } from "../ui/copy/describeFailure";

  let { afterMs = 4000 }: { afterMs?: number } = $props();

  let shown = $state(false);
  let stopping = $state(false);
  let note = $state<string | null>(null);
  $effect(() => {
    const timer = setTimeout(() => (shown = true), afterMs);
    return () => clearTimeout(timer);
  });

  async function stop() {
    stopping = true;
    note = null;
    try {
      const stopped = await repoStore.cancelHooks();
      note = stopped > 0 ? "Stopping…" : "Nothing is running a hook right now.";
    } catch (error) {
      const failure = describeFailure({ verb: "stop", object: "the hook" }, error);
      note = failure.detail ? `${failure.headline} ${failure.detail}` : failure.headline;
    } finally {
      stopping = false;
    }
  }
</script>

{#if shown}
  <div role="status" class="flex items-center gap-2 text-meta text-textMuted">
    <span class="min-w-0 flex-1">Still running. A repository hook (pre-commit, commit-msg, …) may be what is taking the time.</span>
    <button type="button" class="gp-btn shrink-0" data-size="sm" disabled={stopping} onclick={() => void stop()}>
      <Square size={10} /> Stop
    </button>
  </div>
  {#if note}<p class="text-meta text-textMuted">{note}</p>{/if}
{/if}
