<script lang="ts">
  /**
   * Writes the next turn. Enter sends, Shift+Enter is a new line. While a turn
   * runs, what is sent waits in a visible queue (removable) and goes as one
   * turn when the agent is free; nothing is pasted into a busy agent. Stop
   * interrupts the turn; Esc does too, but only from an empty composer, so
   * dismissing a menu never stops an agent.
   */
  import { ArrowUp, Square, X, ShieldAlert } from "@lucide/svelte";

  let {
    disabled = false,
    working,
    queue,
    pendingApprovals,
    modeLabel,
    placeholder = "Ask for a change, or say what to look at",
    onSend,
    onStop,
    onUnqueue,
    onReviewApproval,
  }: {
    disabled?: boolean;
    working: boolean;
    queue: readonly string[];
    pendingApprovals: number;
    modeLabel: string;
    placeholder?: string;
    onSend: (text: string) => Promise<void>;
    onStop: () => Promise<void>;
    onUnqueue: (index: number) => void;
    onReviewApproval: () => void;
  } = $props();

  let text = $state("");
  let error = $state<string | null>(null);
  let sending = $state(false);
  let field: HTMLTextAreaElement | undefined = $state();

  /** Grows the box with its text, up to a limit, then it scrolls. */
  function fit() {
    if (!field) return;
    field.style.height = "auto";
    field.style.height = `${Math.min(field.scrollHeight, 240)}px`;
  }

  async function send() {
    const value = text.trim();
    if (!value || sending || disabled) return;
    sending = true;
    error = null;
    try {
      await onSend(value);
      text = "";
      queueMicrotask(fit);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      sending = false;
    }
  }

  async function stop() {
    error = null;
    try {
      await onStop();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      void send();
    } else if (event.key === "Escape" && working && !text) {
      event.preventDefault();
      void stop();
    }
  }

  export function focus() {
    field?.focus();
  }
</script>

<div class="mavi-composer-wrap">
  <div class="mavi-lane">
    {#if pendingApprovals > 0}
      <button class="mavi-dock gp-notice w-full text-left flex items-center gap-2" data-tone="warn" onclick={onReviewApproval}>
        <ShieldAlert size={13} class="text-warn shrink-0" aria-hidden="true" />
        <span class="text-xs">
          {pendingApprovals === 1 ? "The agent is waiting for your answer." : `${pendingApprovals} requests are waiting for your answer.`}
        </span>
        <span class="ml-auto text-meta text-accent">Review</span>
      </button>
    {/if}
    {#if queue.length}
      <ul class="mavi-queue" aria-label="Queued messages">
        {#each queue as item, i (i)}
          <li class="flex items-center gap-2 text-meta text-textMuted">
            <span class="gp-chip" data-kind="label">Queued</span>
            <span class="truncate flex-1">{item}</span>
            <button class="gp-icon-btn" data-size="sm" aria-label="Remove from queue" onclick={() => onUnqueue(i)}>
              <X size={11} aria-hidden="true" />
            </button>
          </li>
        {/each}
      </ul>
    {/if}
    <div class="mavi-composer gp-card" class:mavi-composer-disabled={disabled}>
      <textarea
        bind:this={field}
        bind:value={text}
        rows="1"
        class="mavi-input text-sm text-textPrimary"
        {placeholder}
        aria-label="Message to the agent"
        {disabled}
        onkeydown={onKeydown}
        oninput={fit}
      ></textarea>
      <div class="flex items-center gap-2">
        <span class="gp-chip" data-kind="label" title="Permission mode, chosen when the thread started">{modeLabel}</span>
        {#if working}<span class="text-micro text-textMuted">Enter queues · Esc stops</span>{/if}
        {#if error}<span class="text-meta text-danger truncate" role="alert">{error}</span>{/if}
        <span class="ml-auto"></span>
        {#if working}
          <button class="gp-btn" data-variant="secondary" onclick={stop} title="Stop this turn">
            <Square size={11} aria-hidden="true" /> Stop
          </button>
        {/if}
        <button
          class="gp-btn"
          data-variant="primary"
          disabled={disabled || sending || !text.trim()}
          onclick={send}
          title={working ? "Queue for when this turn ends" : "Send"}
        >
          <ArrowUp size={12} aria-hidden="true" /> {working ? "Queue" : "Send"}
        </button>
      </div>
    </div>
  </div>
</div>

<style>
  .mavi-composer-wrap {
    padding: 8px 0 12px;
    background: linear-gradient(to bottom, transparent, rgb(var(--c-bg)) 18px);
  }
  .mavi-lane {
    max-width: 46rem;
    margin: 0 auto;
    padding: 0 20px;
  }
  .mavi-composer {
    padding: 8px 10px;
  }
  .mavi-composer-disabled {
    opacity: 0.6;
  }
  .mavi-input {
    display: block;
    width: 100%;
    resize: none;
    background: transparent;
    border: 0;
    outline: none;
    padding: 2px 2px 8px;
    min-height: 1.5em;
    -webkit-text-fill-color: rgb(var(--c-text));
  }
  .mavi-input::placeholder {
    color: rgb(var(--c-text-faint));
    -webkit-text-fill-color: rgb(var(--c-text-faint));
  }
  .mavi-queue {
    margin-bottom: 6px;
    display: grid;
    gap: 2px;
  }
  .mavi-dock {
    margin: 0 0 6px;
  }
</style>
