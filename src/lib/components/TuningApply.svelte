<script lang="ts">
  /**
   * The Storage panel's "Repository tuning" card (owner decision, 2026-11-06).
   *
   * Lists only the changes that clear their measured threshold, and applies
   * nothing until the confirm click below: it shows every exact command with
   * what it writes, where, and how to undo it first. See `health/tuningApply.ts`
   * for the flow.
   */
  import { onDestroy } from "svelte";
  import { invoke } from "../ipc/invoke";
  import type { TuningAdvice, TuningChange, TuningScope } from "../storage/types";
  import {
    createTuningApplyFlow,
    offersTuning,
    type TuningApplyStep,
  } from "../health/tuningApply";

  let {
    advice,
    repoPath,
    onapplied,
  }: {
    advice: TuningAdvice | null | undefined;
    repoPath: string;
    onapplied: () => void;
  } = $props();

  const flow = createTuningApplyFlow({
    repoPath: () => repoPath,
    invoke,
    onApplied: () => onapplied(),
  });
  let current = $state<TuningApplyStep>({ phase: "idle" });
  const unsubscribe = flow.subscribe((next) => {
    current = next;
  });
  onDestroy(unsubscribe);

  // Everything offered is selected until the person unticks it.
  let unticked = $state<TuningChange[]>([]);
  let selected = $derived(
    (advice?.offers ?? []).map((o) => o.change).filter((change) => !unticked.includes(change)),
  );
  let offered = $derived(offersTuning(advice));
  let warnings = $derived(advice?.warnings ?? []);

  const SCOPE_LABEL: Record<TuningScope, string> = {
    repo_config: "this repository's config (shared by its worktrees)",
    global_config: "your global git config",
    os_scheduler: "the OS scheduler (launchd, cron or a systemd timer)",
  };

  function toggle(change: TuningChange) {
    unticked = unticked.includes(change)
      ? unticked.filter((c) => c !== change)
      : [...unticked, change];
  }

  // A panel mounted while an apply is running shows it and can stop it; this
  // only follows the scan and never starts anything.
  $effect(() => {
    flow.syncInFlight(!!advice?.apply_in_flight);
  });
</script>

{#if current.phase !== "idle" || offered || warnings.length > 0}
  <div
    class="rounded-2xl border border-border/70 bg-surface p-3 space-y-2"
    data-testid="tuning-apply"
    data-phase={current.phase}
  >
    {#if current.phase === "idle" || current.phase === "previewing"}
      <div class="text-[11px] font-semibold text-textSecondary">Repository tuning</div>
      {#each warnings as warning}
        <p class="text-[11px] text-amber-300" data-testid="tuning-warning">{warning}</p>
      {/each}
      {#if offered}
        <ul class="space-y-1">
          {#each advice?.offers ?? [] as offer (offer.change)}
            <li class="flex items-start gap-2 text-[11px]" data-testid="tuning-offer">
              <input
                type="checkbox"
                class="mt-0.5"
                checked={!unticked.includes(offer.change)}
                aria-label={offer.title}
                onchange={() => toggle(offer.change)}
              />
              <span>
                <span class="text-textPrimary">{offer.title}</span>
                <span class="block text-textMuted">{offer.reason}</span>
              </span>
            </li>
          {/each}
        </ul>
        <button
          type="button"
          class="rounded-md border border-border px-2 py-1 text-[11px] text-textPrimary hover:bg-surfaceHover disabled:opacity-50"
          data-testid="tuning-apply-open"
          disabled={current.phase === "previewing" || selected.length === 0}
          onclick={() => void flow.open(selected)}
        >
          Preview changes…
        </button>
      {/if}
    {:else if current.phase === "confirming"}
      <p class="text-[11px] text-textMuted">
        These commands run in this repository. Nothing else is written, and each can be undone
        with the command shown under it. Stops after {current.plan.timeout_secs} s.
      </p>
      <ol class="space-y-2">
        {#each current.plan.steps as step (step.change)}
          <li class="space-y-1" data-testid="tuning-step" data-skipped={step.skipped !== null}>
            {#if step.skipped !== null}
              <p class="text-[11px] text-textSecondary" data-testid="tuning-step-skipped">
                Skipped: {step.skipped}.
              </p>
            {:else}
              <code
                class="block select-all font-mono text-[11px] text-textPrimary"
                data-testid="tuning-step-argv">{step.argv.join(" ")}</code
              >
              <ul class="text-[11px] text-textMuted">
                {#each step.writes as write}
                  <li data-testid="tuning-step-write">
                    Writes {write.what} to {SCOPE_LABEL[write.scope]}.
                  </li>
                {/each}
              </ul>
              {#each step.notes as note}
                <p class="text-[11px] text-amber-300" data-testid="tuning-step-note">{note}</p>
              {/each}
              <p class="text-[11px] text-textMuted">Undo:</p>
              {#each step.undo as undo}
                <code
                  class="block select-all font-mono text-[11px] text-textPrimary"
                  data-testid="tuning-step-undo">{undo.join(" ")}</code
                >
              {/each}
              {#if step.undo_note}
                <p class="text-[11px] text-textMuted" data-testid="tuning-step-undo-note">
                  {step.undo_note}
                </p>
              {/if}
            {/if}
          </li>
        {/each}
      </ol>
      <div class="flex gap-2">
        <button
          type="button"
          class="rounded-md bg-accent px-2 py-1 text-[11px] font-semibold text-white"
          data-testid="tuning-apply-confirm"
          onclick={() => void flow.confirm()}
        >
          Apply
        </button>
        <button
          type="button"
          class="rounded-md border border-border px-2 py-1 text-[11px] text-textSecondary"
          data-testid="tuning-apply-cancel"
          onclick={() => void flow.cancel()}
        >
          Cancel
        </button>
      </div>
    {:else if current.phase === "running"}
      <p class="text-[11px] text-textMuted" role="status">
        {current.cancelling ? "Stopping…" : "Applying…"}
      </p>
      <button
        type="button"
        class="rounded-md border border-border px-2 py-1 text-[11px] text-textSecondary disabled:opacity-50"
        data-testid="tuning-apply-stop"
        disabled={current.cancelling}
        onclick={() => void flow.cancel()}
      >
        Stop
      </button>
    {:else if current.phase === "done"}
      <p class="text-[11px] text-textSecondary" role="status" data-testid="tuning-apply-result">
        {#if current.outcome.cancelled}Stopped.{:else if current.outcome.joined}Joined an apply
          already running here.{:else}Finished in {current.outcome.elapsed_ms} ms.{/if}
      </p>
      <ul class="text-[11px] text-textMuted">
        {#each current.outcome.results as result}
          <li data-testid="tuning-result" data-status={result.status}>
            {result.status.replace("_", " ")}: {result.argv.length > 0 ? result.argv.join(" ") : result.change}{result.detail ? ` (${result.detail})` : ""}
          </li>
        {/each}
      </ul>
      <button
        type="button"
        class="rounded-md border border-border px-2 py-1 text-[11px] text-textSecondary"
        onclick={() => flow.dismiss()}
      >
        Dismiss
      </button>
    {:else}
      <p class="text-[11px] text-amber-300" role="alert" data-testid="tuning-apply-error">
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
