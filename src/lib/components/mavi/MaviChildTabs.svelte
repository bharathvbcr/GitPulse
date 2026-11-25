<script lang="ts">
  /**
   * Terminal tabs this thread's agent started, with what each needs. A tab
   * waiting on a permission dialog is otherwise visible only inside itself,
   * while the thread that launched it looks idle. "Open tab" brings the tab
   * on screen; "Ask Mavi to read it" has the agent read the tab and say what
   * it needs, so the person need not leave the thread to find out.
   */
  import { SquareTerminal, ShieldQuestion, CircleCheck, CircleX, X } from "@lucide/svelte";
  import { childTabs, type ChildCard } from "../../mavi/childTabs.svelte";
  import { sessionByNativeId, terminalSessions } from "../../terminal/sessionRegistry";
  import { focusTerminalSession } from "../../terminal/sessionFocus";

  let {
    threadKey,
    onAsk,
  }: {
    threadKey: string;
    /** Sends a message to this thread. */
    onAsk: (text: string) => Promise<void>;
  } = $props();

  let cards = $derived(childTabs.forThread(threadKey));
  let notes = $state<Record<string, string>>({});

  function label(card: ChildCard): string {
    if (card.state === "needs_input") return "Needs your approval";
    if (card.state === "done") return "Finished";
    return card.exitCode !== null && card.exitCode !== 0 ? `Ended (exit ${card.exitCode})` : "Ended";
  }

  async function open(card: ChildCard) {
    const record = sessionByNativeId($terminalSessions, card.sessionId);
    const outcome = await focusTerminalSession(record);
    notes[card.sessionId] = outcome.ok
      ? ""
      : outcome.reason === "no-session"
        ? "That tab has closed."
        : "That tab is not on screen. Open it from the Sessions list.";
  }

  async function ask(card: ChildCard) {
    try {
      await onAsk(`Read terminal tab ${card.sessionId} with gitpulse_terminal_read and tell me what it needs.`);
      notes[card.sessionId] = "";
    } catch (e) {
      notes[card.sessionId] = e instanceof Error ? e.message : String(e);
    }
  }
</script>

{#if cards.length}
  <div class="mavi-children flex flex-col gap-2" role="region" aria-label="Terminal tabs this agent started">
    {#each cards as card (card.sessionId)}
      <section class="mavi-child gp-card" data-state={card.state} data-session={card.sessionId}>
        <header class="flex items-center gap-2 text-xs">
          {#if card.state === "needs_input"}
            <ShieldQuestion size={14} class="text-warn shrink-0" aria-hidden="true" />
          {:else if card.state === "done"}
            <CircleCheck size={14} class="text-textMuted shrink-0" aria-hidden="true" />
          {:else}
            <CircleX size={14} class="text-textMuted shrink-0" aria-hidden="true" />
          {/if}
          <span class="font-medium text-textPrimary truncate">{card.title}</span>
          <span class="text-meta text-textMuted">{label(card)}</span>
          {#if card.state !== "needs_input"}
            <button class="gp-btn ml-auto" data-variant="ghost" aria-label="Dismiss" onclick={() => childTabs.dismiss(threadKey, card.sessionId)}>
              <X size={12} aria-hidden="true" />
            </button>
          {/if}
        </header>
        {#if card.detail}
          <pre class="mavi-child-detail font-mono text-meta select-text">{card.detail}</pre>
        {/if}
        <footer class="flex items-center gap-2 mt-2">
          <button class="gp-btn" data-variant={card.state === "needs_input" ? "primary" : undefined} onclick={() => open(card)}>
            <SquareTerminal size={12} aria-hidden="true" /> Open tab
          </button>
          <button class="gp-btn" onclick={() => ask(card)}>Ask Mavi to read it</button>
          {#if notes[card.sessionId]}<span class="text-meta text-danger" role="alert">{notes[card.sessionId]}</span>{/if}
        </footer>
      </section>
    {/each}
  </div>
{/if}

<style>
  .mavi-child {
    padding: 8px 12px;
  }
  .mavi-child[data-state="needs_input"] {
    border-color: rgb(var(--c-warn) / 0.65);
  }
  .mavi-child-detail {
    margin-top: 6px;
    padding: 6px 8px;
    border-radius: var(--gp-r-sm);
    background: rgb(var(--c-bg) / 0.65);
    border: 1px solid var(--gp-edge);
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 8rem;
    overflow: auto;
  }
</style>
