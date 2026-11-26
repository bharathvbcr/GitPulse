<script lang="ts">
  /**
   * An `AskUserQuestion` from the agent, answered in place.
   *
   * Open: each question shows its options (one choice, or several for a
   * multi-select) and a free-text field. Submit is enabled only once every
   * question has an answer. Decline answers nothing and says so to the agent;
   * it is never read as a choice. Nothing here answers on a keystroke.
   *
   * Answered or declined, the same card stays in the timeline with what was
   * chosen, so the conversation shows what the agent was told.
   */
  import { MessageCircleQuestion, Check, X } from "@lucide/svelte";
  import type { ToolItem } from "../../mavi/types";
  import { answersFromResult, buildAnswers, parseQuestions, type Pick } from "../../mavi/questions";

  let {
    tool,
    onSubmit,
    onDecline,
  }: {
    tool: ToolItem;
    /** Present while the request is open. */
    onSubmit?: (answers: Record<string, string>) => Promise<void>;
    onDecline?: () => Promise<void>;
  } = $props();

  let questions = $derived(parseQuestions(tool.input) ?? []);
  let open = $derived(tool.approval?.state === "open" && onSubmit !== undefined);
  let picks = $state<Pick[]>([]);
  let busy = $state(false);
  let error = $state<string | null>(null);

  function pickOf(index: number): Pick {
    return picks[index] ?? { labels: [], other: "" };
  }
  function choose(index: number, label: string, multi: boolean) {
    const current = pickOf(index);
    let labels: string[];
    if (multi) {
      labels = current.labels.includes(label) ? current.labels.filter((l) => l !== label) : [...current.labels, label];
    } else {
      labels = [label];
    }
    // Choosing a listed option of a single-choice question drops free text.
    picks[index] = { labels, other: multi ? current.other : "" };
  }
  function typeOther(index: number, value: string, multi: boolean) {
    const current = pickOf(index);
    picks[index] = { labels: multi ? current.labels : value.trim() ? [] : current.labels, other: value };
  }

  let answers = $derived(buildAnswers(questions, questions.map((_, i) => picks[i])));

  async function run(action: () => Promise<void>) {
    if (busy) return;
    busy = true;
    error = null;
    try {
      await action();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  let given = $derived(answersFromResult(tool.result));
  let declined = $derived(tool.status === "denied" || tool.approval?.state === "denied");
</script>

<section
  class="mavi-question gp-card"
  aria-label="Question from the agent"
  data-request={tool.approval?.requestId}
  data-state={open ? "open" : declined ? "declined" : "answered"}
  tabindex="-1"
>
  <header class="flex items-center gap-2 text-xs">
    <MessageCircleQuestion size={14} class="{open ? 'text-warn' : 'text-textMuted'} shrink-0" aria-hidden="true" />
    <span class="font-medium text-textPrimary">
      {open ? "The agent is asking you" : declined ? "You declined to answer" : "You answered"}
    </span>
  </header>

  {#each questions as q, i (q.question)}
    <fieldset class="mt-2 mavi-q" disabled={!open || busy}>
      <legend class="text-sm text-textPrimary select-text">
        {#if q.header}<span class="mavi-q-head text-meta text-textMuted">{q.header}</span>{/if}
        {q.question}
      </legend>
      {#if open}
        <div class="mt-1 flex flex-col gap-1">
          {#each q.options as option (option.label)}
            <label class="mavi-option flex items-start gap-2 text-xs">
              <input
                type={q.multiSelect ? "checkbox" : "radio"}
                name="q{i}-{tool.id}"
                checked={pickOf(i).labels.includes(option.label)}
                onchange={() => choose(i, option.label, q.multiSelect)}
              />
              <span>
                <span class="text-textPrimary">{option.label}</span>
                {#if option.description}<span class="block text-meta text-textMuted">{option.description}</span>{/if}
              </span>
            </label>
          {/each}
          <input
            class="gp-field w-full text-xs"
            type="text"
            aria-label="Other answer for: {q.question}"
            placeholder="Other (type your own answer)"
            maxlength="2000"
            value={pickOf(i).other}
            oninput={(e) => typeOther(i, e.currentTarget.value, q.multiSelect)}
          />
        </div>
      {:else if given}
        <p class="mt-1 text-xs text-textSecondary select-text">{given.find((g) => g.question === q.question)?.answer ?? "No answer recorded"}</p>
      {/if}
    </fieldset>
  {/each}

  {#if !open}
    {#if declined}
      <p class="mt-1 text-meta text-textMuted">Nothing was chosen. The agent was told this is not consent.</p>
    {:else if !given && tool.result}
      <p class="mt-1 text-xs text-textSecondary whitespace-pre-wrap select-text">{tool.result}</p>
    {/if}
  {:else}
    <footer class="flex items-center gap-2 mt-2">
      <button
        class="gp-btn"
        data-variant="primary"
        disabled={busy || answers === null}
        onclick={() => answers && run(() => onSubmit!(answers))}
      >
        <Check size={12} aria-hidden="true" /> Send answers
      </button>
      <button class="gp-btn" disabled={busy} onclick={() => run(() => onDecline!())}>
        <X size={12} aria-hidden="true" /> Decline
      </button>
      {#if error}<span class="text-meta text-danger" role="alert">{error}</span>{/if}
    </footer>
  {/if}
</section>

<style>
  .mavi-question {
    padding: 10px 12px;
  }
  .mavi-question[data-state="open"] {
    border-color: rgb(var(--c-warn) / 0.65);
  }
  .mavi-question:focus {
    outline: 2px solid rgb(var(--c-accent));
    outline-offset: 2px;
  }
  .mavi-q {
    border: 0;
    padding: 0;
    margin: 0;
    min-width: 0;
  }
  .mavi-q-head {
    margin-right: 6px;
  }
  .mavi-option {
    cursor: pointer;
  }
</style>
