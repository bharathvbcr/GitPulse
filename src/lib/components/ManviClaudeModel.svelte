<script lang="ts">
  /**
   * Chooses whether Manvi runs on a local model or on Claude through the
   * Anthropic API, and holds the Claude key. GitPulse's own AI features
   * (commit messages, explanations) stay on the local model either way.
   */
  import { onMount } from "svelte";
  import { harnessStore } from "../stores/harnessStore";
  import { CLAUDE_MODELS } from "../workbench/taskModel";
  import { claudeKey, claudeKeyProblem } from "../workbench/claudeKey";
  import { formatDiagnosticFailure } from "../diagnostics/diagnostics";

  let choice = $derived($harnessStore.manviClaude);
  /** null until the Keychain has answered. */
  let keySaved = $state<boolean | null>(null);
  let draft = $state("");
  let working = $state(false);
  let error = $state<string | null>(null);

  async function refresh() {
    try {
      keySaved = await claudeKey.status();
      error = null;
    } catch (err: unknown) {
      error = formatDiagnosticFailure(err);
    }
  }

  onMount(() => {
    void refresh();
  });

  async function run(action: () => Promise<void>) {
    working = true;
    error = null;
    try {
      await action();
      await refresh();
    } catch (err: unknown) {
      error = formatDiagnosticFailure(err);
    } finally {
      working = false;
    }
  }

  function saveKey() {
    const problem = claudeKeyProblem(draft);
    if (problem) {
      error = problem;
      return;
    }
    void run(async () => {
      await claudeKey.save(draft);
      draft = "";
    });
  }

  function useClaude(model: string) {
    harnessStore.selectManviClaude({ model });
  }
</script>

<section class="gp-card p-4 space-y-2">
  <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Manvi's model</h3>
  <div class="flex flex-wrap gap-3 text-[11px]">
    <label class="inline-flex items-center gap-1">
      <input
        type="radio"
        name="manvi-model"
        checked={!choice}
        disabled={working}
        onchange={() => harnessStore.selectManviClaude(null)}
      />
      Local model (below)
    </label>
    <label class="inline-flex items-center gap-1">
      <input
        type="radio"
        name="manvi-model"
        checked={Boolean(choice)}
        disabled={working}
        onchange={() => useClaude(choice?.model ?? CLAUDE_MODELS[1])}
      />
      Claude API
    </label>
  </div>

  {#if choice}
    <label class="flex items-center gap-2 text-[11px]">
      <span class="text-textMuted">Model</span>
      <select
        class="bg-background border border-border rounded px-1.5 py-0.5"
        value={choice.model}
        disabled={working}
        onchange={(event) => useClaude((event.currentTarget as HTMLSelectElement).value)}
      >
        {#each CLAUDE_MODELS as model (model)}
          <option value={model}>{model}</option>
        {/each}
      </select>
    </label>
    <p class="text-[11px] text-textMuted">
      Task suggestions and Manvi's own model calls are billed to this key. Claude Code agents Manvi
      launches keep their own login. Manvi keeps the key from every program it starts.
    </p>
  {/if}

  <div class="space-y-1 text-[11px]">
    {#if keySaved === null}
      <span class="text-textMuted">Checking the Keychain…</span>
    {:else if keySaved}
      <div class="flex items-center gap-2">
        <span>Claude API key saved in the Keychain.</span>
        <button
          class="text-textMuted hover:text-textPrimary underline disabled:opacity-50"
          disabled={working}
          onclick={() => void run(() => claudeKey.remove())}
        >
          Remove
        </button>
      </div>
    {:else if choice}
      <span class="text-amber-400">No Claude API key saved: Manvi cannot run until you add one.</span>
    {/if}
    <form
      class="flex items-center gap-2"
      onsubmit={(event) => {
        event.preventDefault();
        saveKey();
      }}
    >
      <input
        type="password"
        autocomplete="off"
        spellcheck="false"
        placeholder={keySaved ? "Replace with a new key (sk-ant-…)" : "Anthropic API key (sk-ant-…)"}
        class="flex-1 min-w-0 bg-background border border-border rounded px-1.5 py-0.5"
        bind:value={draft}
        disabled={working}
      />
      <button
        type="submit"
        class="px-2 py-0.5 rounded border border-border hover:border-accent disabled:opacity-50"
        disabled={working || !draft.trim()}
      >
        {working ? "Saving…" : "Save key"}
      </button>
    </form>
    {#if error}
      <p class="text-red-400">{error}</p>
    {/if}
  </div>
</section>
