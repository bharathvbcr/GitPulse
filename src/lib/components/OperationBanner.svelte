<script module lang="ts">
  /**
   * Two-stage confirm for the actions that throw work away.
   *
   * Abort discards every conflict resolution the user has typed; skip drops a
   * whole commit. Neither is undoable from this surface, and both sit next to
   * the safe "continue" button — a mis-click is a real loss, so the button
   * arms first and commits second. Exported for tests: the arming rule is the
   * safety property, not the markup around it.
   */
  export function armedLabel(label: string, armed: boolean): string {
    return armed ? `Confirm: ${label}` : label;
  }
</script>

<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { toastStore } from "../stores/toastStore";
  import {
    actionConsequence,
    actionLabel,
    headline,
    isDestructive,
    nextStep,
    orderedActions,
    type OperationAction,
    type OperationState,
  } from "../repos/operation";
  import { AlertTriangle, GitMerge, HelpCircle, Loader2 } from "lucide-svelte";

  // Named `operationState`, not `state`: a prop called `state` shadows the
  // `$state` rune inside the same module and silently turns every reactive
  // declaration below into a store subscription.
  let {
    operationState,
    /** Injected in tests; defaults to the real store action. */
    run = (action: OperationAction) => repoStore.operationAction(action),
  }: {
    operationState: OperationState;
    run?: (action: OperationAction) => Promise<{ ok: boolean; error?: string }>;
  } = $props();

  let busy = $state<OperationAction | null>(null);
  let armed = $state<OperationAction | null>(null);
  let failure = $state<string | null>(null);

  const operation = $derived(operationState.operation);
  const actions = $derived(operation ? orderedActions(operation) : []);

  // Disarm whenever the underlying operation changes shape: an armed "Abort
  // rebase" that survives into a different operation would fire at something
  // the user never looked at.
  let armedFor = $state<string | null>(null);
  $effect(() => {
    const signature = operation
      ? `${operation.kind}:${operation.current_step ?? ""}:${operation.conflicted_total}`
      : "idle";
    if (signature !== armedFor) {
      armedFor = signature;
      armed = null;
    }
  });

  async function activate(action: OperationAction) {
    if (busy) return;
    if (isDestructive(action) && armed !== action) {
      armed = action;
      return;
    }
    armed = null;
    busy = action;
    failure = null;
    try {
      const outcome = await run(action);
      if (!outcome.ok) {
        failure = outcome.error ?? "The operation could not be completed.";
        toastStore.error(failure);
      }
    } finally {
      // Always released: a stuck spinner would leave the only escape from a
      // parked repository permanently disabled.
      busy = null;
    }
  }
</script>

{#if operationState.probeFailed}
  <!--
    The probe itself failed. Saying nothing here would render exactly like a
    clean repository, so the unknown state is stated outright.
  -->
  <div
    class="gp-card flex items-start gap-2.5 rounded-xl border-amber-500/40 bg-amber-500/5 px-3.5 py-2.5"
    role="status"
  >
    <HelpCircle size={15} class="mt-0.5 shrink-0 text-amber-600 dark:text-amber-400" />
    <div class="min-w-0">
      <p class="text-xs font-semibold text-textPrimary">
        Repository state unknown
      </p>
      <p class="mt-0.5 text-[11px] leading-relaxed text-textMuted">
        GitPulse could not check whether a merge, rebase or cherry-pick is in
        progress here. Run <code class="rounded bg-surfaceHover px-1">git status</code>
        in the Terminal view before making changes.
      </p>
    </div>
  </div>
{:else if operation}
  <div
    class="gp-card gp-pop rounded-xl border-accent/40 bg-accent/[0.06] px-3.5 py-3"
    role="status"
    aria-live="polite"
  >
    <div class="flex items-start gap-2.5">
      <GitMerge size={15} class="mt-0.5 shrink-0 text-accent" />
      <div class="min-w-0 flex-1">
        <p class="text-xs font-semibold text-textPrimary">
          {headline(operation)}
        </p>
        <p class="mt-0.5 text-[11px] leading-relaxed text-textMuted">
          {nextStep(operation)}
        </p>

        {#if operation.incoming_ref}
          <p class="mt-1 truncate text-[11px] text-textMuted">
            Applying <span class="font-mono text-textPrimary">{operation.incoming_ref}</span>
          </p>
        {/if}

        {#if operation.warnings && operation.warnings.length > 0}
          <!--
            A degraded read is shown, not swallowed: the difference between
            "no conflicts" and "the conflict list could not be read" decides
            whether continuing is safe.
          -->
          <ul class="mt-1.5 space-y-0.5">
            {#each operation.warnings as warning (warning)}
              <li class="flex items-start gap-1.5 text-[11px] text-amber-600 dark:text-amber-400">
                <AlertTriangle size={11} class="mt-0.5 shrink-0" />
                <span class="min-w-0 break-words">{warning}</span>
              </li>
            {/each}
          </ul>
        {/if}

        {#if failure}
          <p class="mt-1.5 text-[11px] leading-relaxed text-red-600 dark:text-red-400">{failure}</p>
        {/if}

        <div class="mt-2.5 flex flex-wrap items-center gap-1.5">
          {#each actions as action (action)}
            {@const label = actionLabel(operation.kind, action)}
            {@const isArmed = armed === action}
            <button
              type="button"
              onclick={() => activate(action)}
              disabled={busy !== null}
              title={actionConsequence(operation.kind, action)}
              aria-label={armedLabel(label, isArmed)}
              class="{action === 'continue'
                ? 'gp-btn-primary'
                : 'gp-btn'} !py-1 !px-2.5 !text-[11px] inline-flex items-center gap-1.5 {isArmed
                ? '!border-red-500/60 !text-red-600 dark:!text-red-400'
                : ''}"
            >
              {#if busy === action}
                <Loader2 size={11} class="animate-spin" />
              {/if}
              <span>{armedLabel(label, isArmed)}</span>
            </button>
          {/each}

          {#if armed}
            <button
              type="button"
              onclick={() => (armed = null)}
              class="gp-btn !py-1 !px-2.5 !text-[11px]"
            >
              Cancel
            </button>
          {/if}
        </div>

        {#if armed}
          <p class="mt-1.5 text-[11px] leading-relaxed text-red-600 dark:text-red-400">
            {actionConsequence(operation.kind, armed)}
          </p>
        {/if}
      </div>
    </div>
  </div>
{/if}
