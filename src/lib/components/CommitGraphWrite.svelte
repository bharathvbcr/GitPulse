<script lang="ts">
  /**
   * The Storage panel's "Write commit-graph" action (Owner Decision 3).
   *
   * Offered only when the detector says a write would help, and only ever run
   * by the confirm click below: it shows the exact command and the directory it
   * writes under first. See `health/commitGraphWrite.ts` for the flow.
   */
  import { onDestroy } from "svelte";
  import { invoke } from "../ipc/invoke";
  import type { CommitGraphReport } from "../storage/types";
  import {
    createCommitGraphWriteFlow,
    offersCommitGraphWrite,
    type CommitGraphWriteStep,
  } from "../health/commitGraphWrite";

  let {
    report,
    repoPath,
    onwritten,
  }: {
    report: CommitGraphReport | undefined;
    repoPath: string;
    onwritten: () => void;
  } = $props();

  const flow = createCommitGraphWriteFlow({
    repoPath: () => repoPath,
    invoke,
    onWritten: () => onwritten(),
  });
  let current = $state<CommitGraphWriteStep>({ phase: "idle" });
  const unsubscribe = flow.subscribe((next) => {
    current = next;
  });
  onDestroy(unsubscribe);

  let offered = $derived(offersCommitGraphWrite(report));
</script>

{#if current.phase !== "idle" || offered}
  <div
    class="rounded-2xl border border-border/70 bg-surface p-3 space-y-2"
    data-testid="commit-graph-write"
    data-phase={current.phase}
  >
    {#if current.phase === "idle" || current.phase === "previewing"}
      <button
        type="button"
        class="rounded-md border border-border px-2 py-1 text-[11px] text-textPrimary hover:bg-surfaceHover disabled:opacity-50"
        data-testid="commit-graph-write-open"
        disabled={current.phase === "previewing"}
        onclick={() => void flow.open()}
      >
        Write commit-graph…
      </button>
    {:else if current.phase === "confirming"}
      <p class="text-[11px] text-textMuted">
        This runs the command below. It writes derived data under the repository's
        <code class="font-mono">objects/info</code> and changes no git config.
      </p>
      <code
        class="block select-all font-mono text-[11px] text-textPrimary"
        data-testid="commit-graph-write-argv">{current.plan.argv.join(" ")}</code
      >
      <p class="text-[11px] text-textMuted">
        Writes under
        <code class="font-mono text-textPrimary" data-testid="commit-graph-write-dir">{current.plan.common_dir}</code>
        (shared by every worktree of this repository). Stops after {current.plan.timeout_secs} s.
      </p>
      <div class="flex gap-2">
        <button
          type="button"
          class="rounded-md bg-accent px-2 py-1 text-[11px] font-semibold text-white"
          data-testid="commit-graph-write-confirm"
          onclick={() => void flow.confirm()}
        >
          Write commit-graph
        </button>
        <button
          type="button"
          class="rounded-md border border-border px-2 py-1 text-[11px] text-textSecondary"
          data-testid="commit-graph-write-cancel"
          onclick={() => void flow.cancel()}
        >
          Cancel
        </button>
      </div>
    {:else if current.phase === "running"}
      <p class="text-[11px] text-textMuted" role="status">
        {current.cancelling ? "Stopping…" : "Writing the commit-graph…"}
      </p>
      <button
        type="button"
        class="rounded-md border border-border px-2 py-1 text-[11px] text-textSecondary disabled:opacity-50"
        data-testid="commit-graph-write-stop"
        disabled={current.cancelling}
        onclick={() => void flow.cancel()}
      >
        Stop
      </button>
    {:else if current.phase === "done"}
      <p class="text-[11px] text-textSecondary" role="status" data-testid="commit-graph-write-result">
        {#if current.outcome.cancelled}
          Stopped. Nothing was written.
        {:else if current.outcome.joined}
          Joined a write already running in this repository ({current.outcome.elapsed_ms} ms).
        {:else}
          Commit-graph written in {current.outcome.elapsed_ms} ms.
        {/if}
      </p>
      <button
        type="button"
        class="rounded-md border border-border px-2 py-1 text-[11px] text-textSecondary"
        onclick={() => flow.dismiss()}
      >
        Dismiss
      </button>
    {:else}
      <p class="text-[11px] text-amber-300" role="alert" data-testid="commit-graph-write-error">
        {current.message}
      </p>
      <button
        type="button"
        class="rounded-md border border-border px-2 py-1 text-[11px] text-textSecondary"
        onclick={() => flow.dismiss()}
      >
        Dismiss
      </button>
    {/if}
  </div>
{/if}
