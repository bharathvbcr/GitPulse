<script lang="ts">
  /**
   * Chooses whether Manvi runs on a local model or on Claude through the
   * Anthropic API, and shows which credential Claude bills: the machine's
   * `~/.anthropic-accounts` account (key and workspace from one organisation,
   * set up with Manvi's `anthropic-account`) when there is one, else a key
   * saved here. GitPulse's own AI features (commit messages, explanations,
   * branch names, health and coverage plans) follow the same choice.
   */
  import { onMount } from "svelte";
  import { harnessStore } from "../stores/harnessStore";
  import { CLAUDE_MODELS } from "../workbench/taskModel";
  import { claudeKey, claudeKeyProblem, type ClaudeCredentialStatus } from "../workbench/claudeKey";
  import { formatDisplayFailure } from "../diagnostics/report";

  /** What Manvi itself is set to run on (`manvi use`); the controls below write it back. */
  let manvi = $derived($harnessStore.ai?.manvi ?? null);
  let manviReadable = $derived(manvi?.state === "ready");
  /** The Claude choice while Manvi is on cloud, else null. */
  let choice = $derived(manvi?.state === "ready" && manvi.mode === "cloud" ? { model: manvi.model } : null);
  /** null until the backend has answered. */
  let status = $state<ClaudeCredentialStatus | null>(null);
  let keySaved = $derived(status?.keySaved ?? null);
  let account = $derived(status?.account ?? null);
  let accountError = $derived(status?.accountError ?? null);
  /** The registry decides; a key saved here is not used while it exists. */
  let registryInUse = $derived(Boolean(account || accountError));
  let draft = $state("");
  let working = $state(false);
  let error = $state<string | null>(null);

  async function refresh() {
    try {
      status = await claudeKey.status();
      error = null;
    } catch (err: unknown) {
      error = formatDisplayFailure(err, "manvi");
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
      // Saving or removing a key changes what every AI control is ready for.
      if (choice) void harnessStore.refreshClaudeCredential();
    } catch (err: unknown) {
      error = formatDisplayFailure(err, "manvi");
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

  /** `manvi use cloud --model M`: the writer behind the Claude model list. */
  function useClaude(model: string) {
    void run(async () => {
      await harnessStore.useManvi("cloud", { model });
    });
  }

  /** `manvi use local`: switch mode, keeping the server and model Manvi already has. */
  function useLocal() {
    void run(async () => {
      await harnessStore.useManvi("local");
    });
  }
</script>

<section class="gp-card p-4 space-y-2">
  <h3 class="gp-label">Manvi's model</h3>
  <div class="flex flex-wrap gap-3 text-meta">
    <label class="inline-flex items-center gap-1">
      <input
        type="radio"
        name="manvi-model"
        checked={manviReadable && !choice}
        disabled={working || !manviReadable}
        onchange={useLocal}
      />
      Local model (below)
    </label>
    <label class="inline-flex items-center gap-1">
      <input
        type="radio"
        name="manvi-model"
        checked={Boolean(choice)}
        disabled={working || !manviReadable}
        onchange={() => void run(async () => { await harnessStore.useManvi("cloud"); })}
      />
      Claude API
    </label>
  </div>
  {#if manvi && !manviReadable}
    <p class="text-meta text-warn" data-testid="manvi-use-unavailable">{manvi.detail}</p>
  {:else if manvi?.unsupported}
    <p class="text-meta text-warn" data-testid="manvi-use-unsupported">{manvi.unsupported}</p>
  {/if}
  {#if manvi && manviReadable && manvi.shadowed_by.length > 0}
    <p class="text-meta text-textMuted" data-testid="manvi-use-shadowed">
      {manvi.shadowed_by.join(", ")} {manvi.shadowed_by.length === 1 ? "is" : "are"} set in this app's environment and win over the saved choice.
    </p>
  {/if}

  {#if choice}
    <label class="flex items-center gap-2 text-meta">
      <span class="text-textMuted">Model</span>
      <select
        class="gp-select"
        data-size="sm"
        value={choice.model}
        disabled={working || !manviReadable}
        onchange={(event) => useClaude((event.currentTarget as HTMLSelectElement).value)}
      >
        {#each CLAUDE_MODELS as model (model)}
          <option value={model}>{model}</option>
        {/each}
      </select>
    </label>
    <p class="text-meta text-textMuted">
      Task suggestions and Manvi's own model calls are billed to {account ? "this account" : "this key"}.
      GitPulse's own AI features (commit messages, commit explanations, branch names, health and
      coverage plans) use this model too, so their diffs are sent to the Claude API through Manvi.
      Claude Code agents Manvi launches keep their own login. Manvi keeps the key from every program it
      starts.
    </p>
  {/if}

  <div class="space-y-1 text-meta">
    {#if status === null && !error}
      <span class="text-textMuted">Checking for an Anthropic account…</span>
    {:else if account}
      <div class="space-y-0.5" data-testid="manvi-claude-account">
        <div>
          Using Anthropic account <span class="font-mono">{account.name}</span>
          from <span class="font-mono">~/.anthropic-accounts</span>{account.source === "default" ? " (its default)" : ` (${account.source})`}.
        </div>
        <div class="text-textMuted">
          {#if account.workspace}
            Workspace <span class="font-mono">{account.workspace}</span>, sent with that organisation's key.
          {:else}
            No workspace id is recorded for this account; requests use the organisation's default workspace.
          {/if}
        </div>
        <div class="text-textMuted">Change it with <span class="font-mono">anthropic-account</span>; GitPulse never reads the key.</div>
        {#if keySaved}
          <div class="flex items-center gap-2 text-textMuted" data-testid="manvi-claude-unused-key">
            <span>A key is also saved in GitPulse's Keychain entry; it is not used while this account is.</span>
            <button
              class="hover:text-textPrimary underline disabled:opacity-50"
              disabled={working}
              onclick={() => void run(() => claudeKey.remove())}
            >
              Remove
            </button>
          </div>
        {/if}
      </div>
    {:else if accountError}
      <p class="text-danger" data-testid="manvi-claude-account-error">
        ~/.anthropic-accounts exists but couldn't be read, so Manvi will not start on Claude: {accountError}
      </p>
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
    {:else if choice && status}
      <span class="text-warn">
        No Claude API key saved and no ~/.anthropic-accounts registry: Manvi cannot run until you add one.
      </span>
    {/if}
    {#if status && !registryInUse}
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
    {/if}
    {#if error}
      <p class="text-danger">{error}</p>
    {/if}
  </div>
</section>
